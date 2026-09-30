using Microsoft.UI.Xaml;
using NeoSCAD.Host;

namespace NeoSCAD.App;

public partial class App : Application
{
    MainWindow? window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        var arguments = Environment.GetCommandLineArgs().Skip(1).ToArray();
        if (StartupAction.LogPath(arguments) is { } log && AppLog.Open(log))
        {
            AppLog.Write($"NeoSCAD {typeof(App).Assembly.GetName().Version} on {Environment.OSVersion} " +
                         $"({System.Runtime.InteropServices.RuntimeInformation.ProcessArchitecture}), " +
                         $"args: {string.Join(' ', arguments)}");
            // Exceptions that end the app, or that an async void or a
            // discarded task would otherwise lose: the log is read after
            // the process is gone, so each is written as it happens.
            UnhandledException += (_, e) => AppLog.Write("unhandled (XAML)", e.Exception);
            AppDomain.CurrentDomain.UnhandledException += (_, e) =>
                AppLog.Write($"unhandled (terminating={e.IsTerminating}): {e.ExceptionObject}");
            TaskScheduler.UnobservedTaskException += (_, e) => AppLog.Write("unobserved task", e.Exception);
        }
        // The bundled MCAD is mounted in memory under the app's own
        // directory, as OpenSCAD's `<resources>/libraries`.
        CoreService.ResourceDirectory = AppContext.BaseDirectory;
        var startup = StartupAction.Parse(arguments);
        window = new MainWindow(startup);
        window.Activate();
        AppLog.Write("window activated");
    }
}
