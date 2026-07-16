namespace MachineVisionFabric.Contracts.Dataset;

public sealed class DatasetCaptureOptions
{
    public string PackageRoot { get; set; } = ".\\samples\\packages\\dataset-capture-starter";

    public string DatasetRoot { get; set; } = ".\\artifacts\\datasets";

    public bool CreateSessionOnStartup { get; set; } = true;

    public string SessionPrefix { get; set; } = "session";
}
