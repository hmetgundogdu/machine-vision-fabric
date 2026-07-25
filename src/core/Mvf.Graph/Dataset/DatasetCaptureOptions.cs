namespace Mvf.Graph.Dataset;

public sealed class DatasetCaptureOptions
{
    public string PackageRoot { get; set; } = "packages/inspection-demo";

    public string DatasetRoot { get; set; } = "artifacts/datasets";

    public bool CreateSessionOnStartup { get; set; } = true;

    public string SessionPrefix { get; set; } = "session";
}
