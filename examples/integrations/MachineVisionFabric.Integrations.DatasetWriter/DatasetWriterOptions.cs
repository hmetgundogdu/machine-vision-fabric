namespace MachineVisionFabric.Integrations.DatasetWriter;

/// <summary>
/// Configuration options for the dataset-writer sink module.
/// All paths are resolved relative to the package root unless already absolute.
/// </summary>
public sealed class DatasetWriterOptions
{
    /// <summary>
    /// Root directory where the session folder will be created.
    /// If relative, resolved against the package root.
    /// Defaults to <c>datasets</c> under the package root.
    /// </summary>
    public string OutputRoot { get; set; } = "datasets";

    /// <summary>
    /// Optional prefix for the auto-generated session folder name.
    /// Session folders are named <c>{prefix}-{yyyyMMdd}-{HHmmss}-{ms}</c>.
    /// </summary>
    public string SessionPrefix { get; set; } = "session";

    /// <summary>
    /// When true, include the original source file path in per-frame metadata.
    /// </summary>
    public bool IncludeSourcePathInMetadata { get; set; }
}
