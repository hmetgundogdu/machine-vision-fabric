namespace MachineVisionFabric.Contracts.Runtime;

public sealed record HeadlessBootstrapReport(
    string PackageRoot,
    string DatasetSessionRoot,
    bool SessionCreated,
    int ExpectedFrameCount,
    int DeclaredCameraCount,
    int CapturedFrameCount,
    string SessionMetadataPath,
    bool ProductPresent,
    string ProductPresenceSource,
    string ProductPresenceStrategy,
    string FrameSourceSource,
    string FrameSourceStrategy);
