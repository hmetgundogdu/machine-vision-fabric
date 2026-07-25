using System.Text.Json.Nodes;
using Mvf.Graph.Values;

namespace Mvf.Engine.Values;

/// <summary>
/// The config of a <c>select</c> node.
///
/// <para><see cref="Where"/> is the criterion written in config; it can equally arrive on the
/// <c>criterion</c> port from a <c>value</c> node, or come from a resolver. <see cref="By"/> names the
/// property to compare when the elements are objects — with no <c>by</c> the element itself is
/// compared.</para>
///
/// <para><see cref="Type"/> is the <b>element</b> type of the collection. It is declared rather than
/// inferred from the incoming edge so the node's ports are known before the graph is wired; a
/// disagreement with the producer is then an ordinary <c>pipeline.edge.data-type-mismatch</c>.</para>
/// </summary>
public sealed record SelectPrimitiveConfig(
    string RawMode,
    SelectMode Mode,
    bool ModeKnown,
    string RawType,
    ControlValueType Type,
    bool TypeKnown,
    JsonNode? Where,
    bool HasWhere,
    string? By,
    string? Binding,
    string? Prompt,
    JsonNode? ResolvedCriterion,
    bool HasResolvedCriterion,
    bool CriterionFromEdge)
{
    /// <summary>Where the pre-pass parks the resolved criterion. Not authored by hand.</summary>
    public const string ResolvedCriterionKey = "resolvedCriterion";

    /// <summary>
    /// Set by the pre-pass when a producer is wired to the <c>criterion</c> port. The activator reads it to
    /// decide whether this node's criterion is worth offering as a live tunable: with an edge feeding it,
    /// tuning would be overwritten on the very next cycle, and a control that silently does nothing is
    /// worse than no control.
    /// </summary>
    public const string CriterionFromEdgeKey = "criterionFromEdge";

    public static SelectPrimitiveConfig Read(JsonObject config)
    {
        ArgumentNullException.ThrowIfNull(config);

        var rawMode = ConfigJson.String(config, "mode") ?? "one";
        var modeKnown = SelectModes.TryParse(rawMode, out var mode);

        var rawType = ConfigJson.String(config, "type") ?? "json";
        var typeKnown = ControlValueTypes.TryParse(rawType, out var type);

        var hasWhere = config.ContainsKey("where");
        var hasResolved = config.ContainsKey(ResolvedCriterionKey);

        return new SelectPrimitiveConfig(
            RawMode: rawMode,
            Mode: mode,
            ModeKnown: modeKnown,
            RawType: rawType,
            Type: type,
            TypeKnown: typeKnown,
            Where: hasWhere ? config["where"] : null,
            HasWhere: hasWhere,
            By: ConfigJson.String(config, "by"),
            Binding: ConfigJson.String(config, "binding"),
            Prompt: ConfigJson.String(config, "prompt"),
            ResolvedCriterion: hasResolved ? config[ResolvedCriterionKey] : null,
            HasResolvedCriterion: hasResolved,
            CriterionFromEdge: config[CriterionFromEdgeKey] is JsonValue flag
                               && flag.TryGetValue<bool>(out var wired) && wired);
    }

    /// <summary>The criterion available without asking anyone: the pre-pass's, else the one in config.</summary>
    public bool TryGetStaticCriterion(out JsonNode? criterion)
    {
        if (HasResolvedCriterion) { criterion = ResolvedCriterion; return true; }
        if (HasWhere) { criterion = Where; return true; }

        criterion = null;
        return false;
    }
}
