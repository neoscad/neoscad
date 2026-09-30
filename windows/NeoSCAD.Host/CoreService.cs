// The process's one core (the macOS app's `CoreService.shared`): every
// window shares its session, so parse and geometry caches and the analysed
// libraries are shared too.
//
// Threads. Every core call blocks; a render can take minutes. The quick
// document calls (open, update, edit, close) stay on the UI thread, so
// edits reach the session in the order they were made; everything that
// evaluates goes through `Run`, on the thread pool. The core is built for
// concurrent requests (the session's operations all take `&self`), and a
// newer run of a document cancels the older one inside the core.

using NeoSCAD.Native;

namespace NeoSCAD.Host;

public static class CoreService
{
    static readonly Lazy<(Core? Core, string? Error)> shared = new(Start);

    /// <summary>
    /// Where the bundled libraries are mounted (`resource_dir/libraries`);
    /// set before the first use. The app passes its own directory.
    /// </summary>
    public static string? ResourceDirectory { get; set; }

    /// <summary>The core, or null when it did not start (see <see cref="Error"/>).</summary>
    public static Core? Shared => shared.Value.Core;

    /// <summary>Why the core did not start.</summary>
    public static string? Error => shared.Value.Error;

    static (Core?, string?) Start()
    {
        try
        {
            return (new Core(new CoreConfig(ResourceDirectory, false)), null);
        }
        catch (CoreException e)
        {
            return (null, e.Message);
        }
        catch (DllNotFoundException e)
        {
            return (null, $"the core's library is missing: {e.Message}");
        }
    }

    /// <summary>Run a blocking core call on the thread pool.</summary>
    public static Task<T> Run<T>(Func<T> call) => Task.Run(call);
}
