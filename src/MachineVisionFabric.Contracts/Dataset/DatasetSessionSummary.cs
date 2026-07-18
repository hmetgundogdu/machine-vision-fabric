namespace MachineVisionFabric.Contracts.Dataset;

/// <summary>
/// Summary written to <c>session.json</c> when a dataset sink is flushed.
/// Shared between the writer that produces it and the CLI that inspects it.
/// </summary>
public sealed record DatasetSessionSummary(
    string SessionRoot,
    int FrameCount,
    DateTime FinalizedAtUtc,
    IReadOnlyList<DatasetCaptureRecord> Records);
