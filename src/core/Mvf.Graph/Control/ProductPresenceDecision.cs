namespace Mvf.Graph.Control;

public sealed record ProductPresenceDecision(
    bool ProductPresent,
    string Source,
    string StationId,
    DateTime EvaluatedAtUtc,
    string? Details = null);
