namespace MachineVisionFabric.Contracts.Dataset;

public sealed class DatasetCaptureOptions
{
    public string PackageRoot { get; set; } = "real-world-projects/packages/cognex-dark-capture";

    public string DatasetRoot { get; set; } = "artifacts/datasets";

    public bool CreateSessionOnStartup { get; set; } = true;

    public string SessionPrefix { get; set; } = "session";
}
