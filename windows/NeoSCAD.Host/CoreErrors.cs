using NeoSCAD.Native;

namespace NeoSCAD.Host;

public static class CoreErrors
{
    /// <summary>
    /// The sentence to show for an error from the core. The generated
    /// exceptions' <see cref="Exception.Message"/> reads "@message=...",
    /// the generator's rendering of the variant's field, so the field is
    /// taken instead.
    /// </summary>
    public static string Describe(Exception e) => e switch
    {
        CoreException.Failed f => f.message,
        CoreException.InvalidArgument a => a.message,
        CoreException.Panicked p => $"internal error: {p.message}",
        CoreException.Cancelled => "cancelled",
        _ => e.Message,
    };
}
