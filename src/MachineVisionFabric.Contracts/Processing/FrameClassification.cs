namespace MachineVisionFabric.Contracts.Processing;

/// <summary>
/// The result of classifying a frame's content into a routing label, optionally
/// with a scalar measurement (e.g. mean brightness, object count, a dimension).
/// A classifier turns perception into a control signal that <c>switch</c>/<c>if</c>
/// nodes route on — without touching the data (frame) channel.
/// </summary>
public sealed record FrameClassification(
    string Label,
    string Source,
    DateTime EvaluatedAtUtc,
    double? Measurement = null,
    string? Unit = null,
    string? Details = null);
