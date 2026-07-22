namespace MachineVisionFabric.Integrations.FolderSource;

/// <summary>
/// Configuration for the folder-sequence simulator source. The folder is resolved relative to the
/// package root unless already absolute.
/// </summary>
public sealed class FolderSourceOptions
{
    /// <summary>Folder whose files (in name order) become frames. Default: <c>assets/frames</c>.</summary>
    public string SourceFolder { get; set; } = "assets/frames";

    /// <summary>Replay the folder forever when true; otherwise stop after the last file.</summary>
    public bool Loop { get; set; }

    /// <summary>Delay between frames in milliseconds (0 = as fast as the pipeline pulls).</summary>
    public int FrameIntervalMs { get; set; }
}
