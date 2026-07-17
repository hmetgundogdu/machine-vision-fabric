namespace MachineVisionFabric.Integrations.DatasetWriter;

/// <summary>
/// Configuration for the dataset-writer sink module.
/// Paths are resolved relative to the package root unless already absolute.
/// </summary>
public sealed class DatasetWriterOptions
{
    /// <summary>
    /// Root directory where the timestamped session folder is created.
    /// Relative paths resolve against the package root. Default: <c>datasets</c>.
    /// </summary>
    public string OutputRoot { get; set; } = "datasets";

    /// <summary>
    /// Prefix for the auto-generated session folder name
    /// (<c>{prefix}-{yyyyMMdd}-{HHmmss}-{ms}</c>).
    /// </summary>
    public string SessionPrefix { get; set; } = "session";

    /// <summary>
    /// When true, include the original source path in per-frame metadata.
    /// </summary>
    public bool IncludeSourcePathInMetadata { get; set; }
}
