using System.Text.Json.Nodes;

namespace Mvf.Graph.Execution;

/// <summary>
/// A typed control-channel value emitted by gate, classifier and control nodes.
/// The control channel deliberately carries decisions, scalar measurements and configuration values —
/// never image data. Frames travel only on data edges.
/// </summary>
public sealed class ControlSignal
{
    /// <summary>
    /// Identifies the signal contract.
    /// Well-known values: <c>boolean-gate</c>, <c>classification</c>, <c>measurement</c>,
    /// <c>value</c>, <c>list</c>.
    /// </summary>
    public required string SignalType { get; init; }

    /// <summary>
    /// The boolean gate decision (true = product present / condition met).
    /// Used when SignalType is <c>boolean-gate</c>.
    /// </summary>
    public required bool Value { get; init; }

    /// <summary>
    /// Optional string classification label used by the <c>switch</c> primitive for routing.
    /// Used when SignalType is <c>classification</c>.
    /// Example: "accept", "reject", "quarantine".
    /// </summary>
    public string? ClassLabel { get; init; }

    /// <summary>
    /// Optional scalar measurement carried on the control channel, e.g. a mean brightness,
    /// an object count, or a measured dimension derived from a frame. Used when SignalType is
    /// <c>measurement</c>, and may also accompany a <c>classification</c> for downstream logic.
    /// </summary>
    public double? Measurement { get; init; }

    /// <summary>Unit for <see cref="Measurement"/> (e.g. "px", "mm", "count"). Optional.</summary>
    public string? Unit { get; init; }

    public string Source { get; init; } = string.Empty;

    public string StationId { get; init; } = string.Empty;

    public DateTime TimestampUtc { get; init; } = DateTime.UtcNow;

    public string? Details { get; init; }

    /// <summary>
    /// The payload of a <c>value</c> or <c>list</c> signal — one typed value the graph could not compute,
    /// or a homogeneous collection of them. Its declared port type (<c>control/value:&lt;t&gt;</c> /
    /// <c>control/list:&lt;t&gt;</c>) is what the validator checks; a <c>json</c> element type may also
    /// carry a JSON Schema, enforced wherever the value enters the graph.
    /// </summary>
    public JsonNode? Payload { get; init; }

    /// <summary>One typed value on the control channel.</summary>
    public static ControlSignal FromValue(JsonNode? payload, string source = "") => new()
    {
        SignalType = "value",
        // A boolean signal has no meaning for a value; "did it resolve to something" is the only honest
        // reading, and it keeps a value usable by anything already gating on Value.
        Value = payload is not null,
        Payload = payload,
        Source = source
    };

    /// <summary>A homogeneous collection on the control channel.</summary>
    public static ControlSignal FromList(JsonArray items, string source = "") => new()
    {
        SignalType = "list",
        Value = items.Count > 0,
        Payload = items,
        Source = source
    };
}
