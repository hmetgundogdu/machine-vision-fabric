using MachineVisionFabric.Contracts.Control;

namespace MachineVisionFabric.Contracts.Dataset;

public sealed class DatasetSessionMetadata
{
    public required string PackageName { get; init; }

    public required string SessionRoot { get; init; }

    public required DateTime CreatedAtUtc { get; init; }

    public required int CapturedFrameCount { get; init; }

    public required int DeclaredCameraCount { get; init; }

    public required string Scenario { get; init; }

    public required DatasetCapturePolicy CapturePolicy { get; init; }

    public required ProductPresenceDecision ProductPresenceDecision { get; init; }

    public required IReadOnlyList<DatasetCaptureRecord> Records { get; init; }
}
