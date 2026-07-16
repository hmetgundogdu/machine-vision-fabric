using MachineVisionFabric.Contracts.Control;

namespace MachineVisionFabric.Contracts.Dataset;

public sealed record DatasetCollectionResult(
    int CapturedFrameCount,
    string SessionMetadataPath,
    IReadOnlyList<DatasetCaptureRecord> Records,
    ProductPresenceDecision ProductPresenceDecision);
