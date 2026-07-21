namespace Mvf.Graph.Processing;

public sealed record FrameProcessorDecision(
    bool Accepted,
    string Source,
    string Strategy,
    DateTime EvaluatedAtUtc,
    string? Details);
