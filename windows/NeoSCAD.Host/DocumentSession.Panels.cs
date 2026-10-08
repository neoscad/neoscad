// The window's panels, without the UI: the customizer, the check and the
// measure panel, their overlay in the 3D view, and File > Export. The port
// of the macOS app's DocumentLoop.swift ("The customizer"), Inspect.swift
// and Export.swift; what each panel computes (an edit's override, the
// check's findings, a measurement, the overlay's lines) is the core's
// (`client` through crates/ffi), so the three apps agree.
//
// Editing a customizer value never touches the text: the loop keeps the
// edited values and every run passes them as `-D`-style assignments after
// the text, as OpenSCAD's customizer does.
//
// Check, measure and export run detached from the document loop
// (crates/ffi/src/inspect.rs): a check does not cancel the live preview,
// and typing does not cancel an export. A newer check or measurement
// cancels the older one of its own kind.
//
// Threads: as DocumentSession, every member is called on the UI thread;
// the core's long calls go to the thread pool and come back through the
// dispatcher.

using NeoSCAD.Native;

namespace NeoSCAD.Host;

public sealed partial class DocumentSession
{
    // --- Shared by the panels ----------------------------------------------------

    /// <summary>
    /// The core's path for this document with the current text sent: the
    /// first step of a run, without the run, for a panel request made
    /// before any run or after the copies fell out of step.
    /// </summary>
    string PanelPath()
    {
        if (core is null) throw new CoreException.Failed($"The core did not start: {CoreService.Error}");
        var path = CorePath;
        var state = loop.State();
        if (state.Path != path || !state.InSync)
        {
            CloseInCore(loop.SetPath(path));
            core.Update(path, storage.Text());
            loop.TextSent();
        }
        return path;
    }

    /// <summary>What a detached request runs with: the customizer's values, the parts toggle and the extensions.</summary>
    RunOptions PanelRunOptions() => new(loop.Overrides(), loop.State().Parts, enable);

    /// <summary>
    /// Run a blocking core call on the thread pool and finish on the UI
    /// thread, through the dispatcher rather than the awaiting context, so
    /// tests (which have no synchronization context) see the same order
    /// as the app.
    /// </summary>
    Task<T> Background<T>(Func<T> call)
    {
        var done = new TaskCompletionSource<T>();
        CoreService.Run(call).ContinueWith(t => ui.Post(() =>
        {
            if (t.Exception is { } x) done.SetException(x.InnerExceptions);
            else done.SetResult(t.Result);
        }), TaskScheduler.Default);
        return done.Task;
    }

    void CancelPanels()
    {
        CancelQuietly(checkCancel);
        CancelQuietly(measureCancel);
        measurement?.Measurement?.Dispose();
    }

    static void CancelQuietly(CancelToken? token)
    {
        try
        {
            token?.Cancel();
        }
        catch (CoreException)
        {
        }
    }

    // --- The customizer -----------------------------------------------------------

    int parameterGeneration;

    /// <summary>The text's parameters, grouped as the file groups them; empty when it has none.</summary>
    public IReadOnlyList<ParameterGroup> ParameterGroups { get; private set; } = [];

    /// <summary>The values edited away from the text's, by parameter name.</summary>
    public IReadOnlyDictionary<string, ParameterValue> ParameterValues { get; private set; } =
        new Dictionary<string, ParameterValue>();

    /// <summary>The parameter sets in the JSON file beside the model.</summary>
    public IReadOnlyList<string> ParameterSets { get; private set; } = [];

    /// <summary>The set last applied, until a value is edited.</summary>
    public string? SelectedParameterSet { get; private set; }

    /// <summary>
    /// OpenSCAD's parameter sets file, `name.json` beside `name.scad`; null
    /// while the document is untitled (there is nowhere to keep sets).
    /// </summary>
    public string? ParameterSetPath
    {
        get
        {
            if (FilePath is not { } p) return null;
            try
            {
                return NeoScad.ParameterSetPath(p);
            }
            catch (CoreException)
            {
                return null;
            }
        }
    }

    /// <summary>The parameters themselves changed (a new text): the panel rebuilds its controls.</summary>
    public event Action? ParametersChanged;

    /// <summary>Edited values or the set list changed: the panel updates what it shows.</summary>
    public event Action? ParameterValuesChanged;

    /// <summary>A parameter's value as the customizer shows it: the edited one, or the text's.</summary>
    public ParameterValue ValueOf(Parameter p) =>
        ParameterValues.TryGetValue(p.Name, out var v) ? v : p.DefaultValue;

    /// <summary>Read the parameters of the current text (after every run: the text is the run's).</summary>
    public void RefreshParameters()
    {
        if (closed || core is null) return;
        string path;
        try
        {
            path = PanelPath();
        }
        catch (CoreException)
        {
            return;
        }
        var generation = ++parameterGeneration;
        _ = Background(() => core.Parameters(path)).ContinueWith(t =>
        {
            if (closed || generation != parameterGeneration || t.Exception is not null) return;
            var groups = t.Result;
            if (!ParameterShapes.Same(ParameterGroups, groups))
            {
                ParameterGroups = groups;
                ParametersChanged?.Invoke();
            }
            // Values of parameters that are gone are dropped by the loop.
            try
            {
                loop.ParametersRead(groups);
            }
            catch (CoreException)
            {
            }
            SyncParameterValues();
        }, TaskContinuationOptions.ExecuteSynchronously);
    }

    /// <summary>A control's edit, as the core turns it into an override (snapped, clamped, cut to length).</summary>
    public void EditParameter(Parameter parameter, ParameterEdit edit)
    {
        ParameterValue? value;
        try
        {
            value = NeoScad.EditParameter(parameter, ValueOf(parameter), edit);
        }
        catch (CoreException)
        {
            return;
        }
        SetParameter(parameter.Name, value);
    }

    /// <summary>Set (or with null, clear) one edited value; the document previews after the pause.</summary>
    public void SetParameter(string name, ParameterValue? value)
    {
        if (closed) return;
        try
        {
            if (loop.SetParameter(name, value, clock.NowMs)) ArmTimer();
        }
        catch (CoreException)
        {
        }
        SyncParameterValues();
    }

    /// <summary>Every value back to the text's.</summary>
    public void ResetParameters()
    {
        if (closed) return;
        try
        {
            if (loop.ResetParameters(clock.NowMs)) ArmTimer();
        }
        catch (CoreException)
        {
        }
        SyncParameterValues();
    }

    /// <summary>
    /// Apply a set as OpenSCAD's `-p file -P name` does (values checked and
    /// clamped, unnamed parameters back to the text's). Returns why it
    /// failed, or null.
    /// </summary>
    public string? ApplyParameterSet(string name)
    {
        if (closed || core is null || ParameterSetPath is not { } json) return "Save the document to use parameter sets.";
        try
        {
            var path = PanelPath();
            var values = core.ApplyParameterSet(path, json, name);
            loop.ParameterSetApplied(name, values, [.. ParameterGroups], clock.NowMs);
            ArmTimer();
            SyncParameterValues();
            return null;
        }
        catch (CoreException e)
        {
            return CoreErrors.Describe(e);
        }
    }

    /// <summary>Save the current values as the set <paramref name="name"/> (replacing one of that name).</summary>
    public string? SaveParameterSet(string name)
    {
        name = name.Trim();
        if (name.Length == 0) return "A parameter set needs a name.";
        if (closed || core is null || ParameterSetPath is not { } json) return "Save the document to keep parameter sets beside it.";
        try
        {
            var path = PanelPath();
            var values = ParameterValues.Select(kv => new ParameterOverride(kv.Key, kv.Value)).ToArray();
            core.SaveParameterSet(path, json, name, values);
            RefreshParameterSets();
            SelectedParameterSet = name;
            ParameterValuesChanged?.Invoke();
            return null;
        }
        catch (CoreException e)
        {
            return CoreErrors.Describe(e);
        }
    }

    /// <summary>Read the set names from the file beside the model.</summary>
    public void RefreshParameterSets()
    {
        string[] sets = [];
        if (core is not null && ParameterSetPath is { } json)
        {
            try
            {
                sets = core.ParameterSets(json);
            }
            catch (CoreException)
            {
            }
        }
        ParameterSets = sets;
        ParameterValuesChanged?.Invoke();
    }

    /// <summary>Take the loop's values and selected set into the properties the panel reads.</summary>
    void SyncParameterValues()
    {
        DocumentState state;
        try
        {
            state = loop.State();
        }
        catch (CoreException)
        {
            return;
        }
        var values = new Dictionary<string, ParameterValue>();
        foreach (var o in state.Overrides ?? []) values.TryAdd(o.Name, o.Value);
        var same = values.Count == ParameterValues.Count
                   && values.All(kv => ParameterValues.TryGetValue(kv.Key, out var v)
                                                     && ParameterShapes.Value(v) == ParameterShapes.Value(kv.Value));
        if (same && state.SelectedSet == SelectedParameterSet) return;
        ParameterValues = values;
        SelectedParameterSet = state.SelectedSet;
        ParameterValuesChanged?.Invoke();
    }

    // --- Check ----------------------------------------------------------------------

    CancelToken? checkCancel;

    /// <summary>The printer the check measures against (the panel edits it).</summary>
    public PrinterSettings CheckSettings { get; set; } = DefaultPrinter();

    /// <summary>Check again after every render (not every preview: a check renders too).</summary>
    public bool CheckAfterRender { get; set; }

    public CheckReport? CheckReport { get; private set; }
    public string? CheckError { get; private set; }
    public bool CheckRunning { get; private set; }

    /// <summary>The finding picked out in the view, by id.</summary>
    public uint? SelectedFinding { get; private set; }

    public event Action? CheckChanged;

    static PrinterSettings DefaultPrinter()
    {
        try
        {
            return NeoScad.DefaultPrinterSettings();
        }
        catch (Exception e) when (e is CoreException or DllNotFoundException or TypeInitializationException)
        {
            return new PrinterSettings("", 0.4, 0.8, 45, false, [220, 220, 250]);
        }
    }

    /// <summary>
    /// Check the current text with the customizer's values for FDM
    /// printing (`neoscad check`); a newer check cancels this one.
    /// </summary>
    public async Task RunCheckAsync()
    {
        if (closed) return;
        string path;
        CheckOptions options;
        try
        {
            path = PanelPath();
            options = NeoScad.PrinterCheckOptions(CheckSettings);
        }
        catch (CoreException e)
        {
            CheckError = CoreErrors.Describe(e);
            CheckChanged?.Invoke();
            return;
        }
        CancelQuietly(checkCancel);
        var cancel = checkCancel = new CancelToken();
        var run = PanelRunOptions();
        CheckRunning = true;
        CheckChanged?.Invoke();
        try
        {
            var r = await Background(() => core!.Check(path, options, run, cancel));
            if (closed || cancel != checkCancel) return;
            CheckReport = r;
            CheckError = r.Failed && r.Findings.Length == 0 ? NeoScad.FirstError(r.Console) : null;
            if (SelectedFinding is { } s && !r.Findings.Any(f => f.Id == s)) SelectedFinding = null;
        }
        catch (CoreException.Cancelled)
        {
            return;
        }
        catch (CoreException e)
        {
            if (cancel != checkCancel) return;
            CheckError = CoreErrors.Describe(e);
        }
        CheckRunning = false;
        CheckChanged?.Invoke();
        UpdateOverlay();
    }

    /// <summary>Pick out a finding in the view (its box), turned to its point; null clears it.</summary>
    public void SelectFinding(uint? id)
    {
        SelectedFinding = id;
        if (id is { } i && CheckReport?.Findings.FirstOrDefault(f => f.Id == i) is { Point.Length: 3 } f)
        {
            try
            {
                Viewport?.LookAt(f.Point);
            }
            catch (CoreException)
            {
            }
        }
        CheckChanged?.Invoke();
        UpdateOverlay();
    }

    /// <summary>One line for the panel's header ("2 errors, 1 warning", or "No problems found").</summary>
    public string CheckSummary()
    {
        if (CheckReport is not { } r) return "";
        try
        {
            return NeoScad.CheckSummary(r);
        }
        catch (CoreException)
        {
            return "";
        }
    }

    // --- Measure --------------------------------------------------------------------

    CancelToken? measureCancel;
    MeasureResult? measurement;
    readonly List<double[]> picks = [];

    public MeasureResult? Measurement => measurement;
    public string? MeasureError { get; private set; }
    public bool MeasureRunning { get; private set; }

    /// <summary>Clicks in the view pick points on the model (two give a distance).</summary>
    public bool Picking { get; set; }

    /// <summary>The picked points, in model coordinates (at most two).</summary>
    public IReadOnlyList<double[]> Picks => picks;

    /// <summary>The distance between the two picked points.</summary>
    public double? PickedDistance
    {
        get
        {
            try
            {
                return NeoScad.PickDistance([.. picks]);
            }
            catch (CoreException)
            {
                return null;
            }
        }
    }

    public event Action? MeasureChanged;

    /// <summary>Measure the current text with the customizer's values (`neoscad measure`).</summary>
    public async Task RunMeasureAsync()
    {
        if (closed) return;
        string path;
        try
        {
            path = PanelPath();
        }
        catch (CoreException e)
        {
            MeasureError = CoreErrors.Describe(e);
            MeasureChanged?.Invoke();
            return;
        }
        CancelQuietly(measureCancel);
        var cancel = measureCancel = new CancelToken();
        var run = PanelRunOptions();
        MeasureRunning = true;
        MeasureChanged?.Invoke();
        try
        {
            var r = await Background(() => core!.Measure(path, run, cancel));
            if (closed || cancel != measureCancel) return;
            measurement?.Measurement?.Dispose();
            measurement = r;
            MeasureError = r.ExitCode != 0 ? NeoScad.FirstError(r.Console) ?? "The model did not render." : null;
            picks.Clear();
        }
        catch (CoreException.Cancelled)
        {
            return;
        }
        catch (CoreException e)
        {
            if (cancel != measureCancel) return;
            MeasureError = CoreErrors.Describe(e);
        }
        MeasureRunning = false;
        MeasureChanged?.Invoke();
        UpdateOverlay();
    }

    /// <summary>
    /// A click in the view at (<paramref name="x"/>, <paramref name="y"/>),
    /// in the view's points from its top left: with picking on, the model's
    /// surface point under it. Whether the click was taken.
    /// </summary>
    public bool PickAt(double x, double y)
    {
        if (!Picking || Viewport is not { } v) return false;
        PickRay? ray;
        try
        {
            ray = v.RayAt(x, y);
        }
        catch (CoreException)
        {
            return false;
        }
        return ray is not null && PickAlong(ray.Origin, ray.Direction);
    }

    /// <summary>
    /// Pick where a ray (model coordinates) first meets the model: a third
    /// pick starts a new pair. Whether picking was on and measured.
    /// </summary>
    public bool PickAlong(double[] origin, double[] direction)
    {
        if (!Picking || measurement?.Measurement is not { } m) return false;
        double[]? hit;
        try
        {
            hit = m.Pick(origin, direction);
        }
        catch (CoreException)
        {
            return true;
        }
        if (hit is null) return true;
        if (picks.Count >= 2) picks.Clear();
        picks.Add(hit);
        MeasureChanged?.Invoke();
        UpdateOverlay();
        return true;
    }

    public void ClearPicks()
    {
        picks.Clear();
        MeasureChanged?.Invoke();
        UpdateOverlay();
    }

    // --- The view's overlay ---------------------------------------------------------

    /// <summary>What the panels draw over the model: every finding (the selected one with its box) and the picks.</summary>
    public OverlayState Overlay() =>
        new(CheckReport?.Findings ?? [], SelectedFinding, null, null, [.. picks]);

    void UpdateOverlay()
    {
        if (Viewport is not { } v) return;
        try
        {
            v.SetOverlay(Overlay());
        }
        catch (CoreException)
        {
        }
    }

    // --- Export -----------------------------------------------------------------------

    /// <summary>The dimension of the last model shown (2 or 3), for File > Export's formats.</summary>
    public uint? LastDimensions =>
        Report is RunReport.Rendered { Result.Geometry: { } g } ? g.Dimensions : null;

    /// <summary>
    /// Export the model to <paramref name="output"/> as <paramref
    /// name="format"/> (an id from <c>NeoScad.ExportFormats()</c>): a full
    /// render of the current text with the customizer's values, on the
    /// thread pool, reporting each stage to <paramref name="stage"/> (on
    /// the core's thread) and stopped by <paramref name="cancel"/>. The
    /// core writes geometry atomically. Returns why it failed, or null.
    /// </summary>
    public async Task<string?> ExportAsync(string output, string format, Action<string>? stage = null,
        CancelToken? cancel = null)
    {
        if (core is null) return $"The core did not start: {CoreService.Error}";
        output = Path.GetFullPath(output);
        try
        {
            var info = Array.Find(NeoScad.ExportFormats(), f => f.Id == format);
            if (info?.Kind == ExportKind.ViewImage) return await ExportImageAsync(output, 1600, 1200);
            var path = PanelPath();
            var run = PanelRunOptions();
            if (info?.Kind == ExportKind.Snapshot)
            {
                var s = await CoreService.Run(() =>
                    core.SnapshotFile(path, new SnapshotOptions(1600, 1200, [], false, false), run, cancel));
                if (s.ExitCode != 0 || s.Png is null)
                    return NeoScad.FirstError(s.Console) ?? $"The snapshot failed (exit code {s.ExitCode}).";
                await File.WriteAllBytesAsync(output, s.Png);
                return null;
            }
            var listener = stage is null ? null : new StageListener(stage);
            var options = new ExportOptions(format, null, null, null);
            var r = await CoreService.Run(() => core.ExportFile(path, output, options, run, cancel, listener));
            return NeoScad.ExportFailureReason(r);
        }
        catch (CoreException.Cancelled)
        {
            return "The export was cancelled.";
        }
        catch (CoreException e)
        {
            return CoreErrors.Describe(e);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return e.Message;
        }
    }

    /// <summary>The current view as a PNG file (File > Export > Image).</summary>
    public async Task<string?> ExportImageAsync(string output, uint width, uint height)
    {
        if (Viewport is not { } v) return "There is no view to export.";
        try
        {
            var png = await CoreService.Run(() => v.Image(width, height));
            await File.WriteAllBytesAsync(output, png);
            return null;
        }
        catch (CoreException e)
        {
            return CoreErrors.Describe(e);
        }
    }

    sealed class StageListener(Action<string> stage) : ProgressListener
    {
        public void Stage(string name) => stage(name);
    }
}

/// <summary>
/// Whether two parameter lists have the same parameters (names,
/// descriptions, controls and the text's values): when they do, the panel
/// keeps its controls (and the focus in them) and only updates values.
/// The binding's records hold arrays, which compare by reference, so the
/// comparison is spelled out.
/// </summary>
public static class ParameterShapes
{
    public static bool Same(IReadOnlyList<ParameterGroup> a, IReadOnlyList<ParameterGroup> b) => Key(a) == Key(b);

    public static string Key(IReadOnlyList<ParameterGroup> groups)
    {
        var s = new System.Text.StringBuilder();
        foreach (var g in groups)
        {
            s.Append("[g]").Append(g.Name).Append('\n');
            foreach (var p in g.Parameters)
            {
                s.Append(p.Name).Append('\u001f').Append(p.Description).Append('\u001f')
                    .Append(Control(p.Control)).Append('\u001f').Append(Value(p.DefaultValue)).Append('\n');
            }
        }
        return s.ToString();
    }

    static string Control(ParameterControl c) => c switch
    {
        ParameterControl.Slider s => $"slider {s.Min} {s.Max} {s.Step}",
        ParameterControl.SpinBox s => $"spin {s.Min} {s.Max} {s.Step}",
        ParameterControl.Text t => $"text {t.MaxLength}",
        ParameterControl.Vector v => $"vector {v.Min} {v.Max} {v.Step}",
        ParameterControl.Dropdown d => "dropdown " + string.Join("|", d.Options.Select(o => o.Label + "=" + Value(o.Value))),
        _ => "checkbox",
    };

    /// <summary>A value as text, for comparing (not for showing).</summary>
    public static string Value(ParameterValue v) => v switch
    {
        ParameterValue.Bool b => b.Value ? "true" : "false",
        ParameterValue.Number n => n.Value.ToString("R", System.Globalization.CultureInfo.InvariantCulture),
        ParameterValue.Text t => "\"" + t.Value,
        ParameterValue.Vector x => "[" + string.Join(",", x.Value.Select(d => d.ToString("R", System.Globalization.CultureInfo.InvariantCulture))),
        _ => "",
    };
}

/// <summary>What an export's progress dialog says for each of the core's stages.</summary>
public static class ExportText
{
    /// <param name="stage">The core's stage name (`parse`, `evaluate`, `geometry`, `draw`); null before the first.</param>
    public static string Describe(string? stage) => stage switch
    {
        null or "" => "Starting…",
        "parse" => "Reading the model…",
        "evaluate" => "Evaluating…",
        "geometry" => "Building the geometry…",
        "draw" => "Drawing…",
        _ => $"{char.ToUpperInvariant(stage[0])}{stage[1..]}…",
    };
}
