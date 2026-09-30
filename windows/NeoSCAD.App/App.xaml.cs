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
        // The bundled MCAD is mounted in memory under the app's own
        // directory, as OpenSCAD's `<resources>/libraries`.
        CoreService.ResourceDirectory = AppContext.BaseDirectory;
        var startup = StartupAction.Parse(Environment.GetCommandLineArgs().Skip(1).ToArray());
        window = new MainWindow(startup);
        window.Activate();
    }
}
