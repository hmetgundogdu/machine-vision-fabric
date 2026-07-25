using System.Text.Json.Nodes;
using Mvf.Graph.Values;

namespace Mvf.Engine.Values;

/// <summary>
/// The config of a <c>value</c> node, read straight off the node's <c>config</c> object.
///
/// <para>Resolution order, first hit wins: <see cref="Literal"/>, then the binding store (when
/// <see cref="Binding"/> is set), then an <c>IValueResolver</c>, then <see cref="Default"/>. The
/// pre-pass writes the winner back as <see cref="ResolvedKey"/> so execution sees a constant.</para>
/// </summary>
public sealed record ValuePrimitiveConfig(
    string RawType,
    ControlValueType Type,
    bool TypeKnown,
    string RawShape,
    ControlValueShape Shape,
    bool ShapeKnown,
    JsonNode? Schema,
    JsonNode? Literal,
    bool HasLiteral,
    string? Binding,
    string? Prompt,
    JsonNode? Default,
    bool HasDefault,
    JsonNode? Resolved,
    bool HasResolved)
{
    /// <summary>Where the pre-pass parks the resolved constant. Not authored by hand.</summary>
    public const string ResolvedKey = "resolved";

    public static ValuePrimitiveConfig Read(JsonObject config)
    {
        ArgumentNullException.ThrowIfNull(config);

        var rawType = ConfigJson.String(config, "type") ?? "string";
        var typeKnown = ControlValueTypes.TryParse(rawType, out var type);

        // "one" (default) or "list". A list is still one value the graph cannot compute — a set of
        // candidates, a set of allowed labels — and it is what feeds a `select`'s items port.
        var rawShape = ConfigJson.String(config, "shape") ?? "one";
        var shapeKnown = TryParseShape(rawShape, out var shape);

        var hasLiteral = config.ContainsKey("literal");
        var hasDefault = config.ContainsKey("default");
        var hasResolved = config.ContainsKey(ResolvedKey);

        return new ValuePrimitiveConfig(
            RawType: rawType,
            Type: type,
            TypeKnown: typeKnown,
            RawShape: rawShape,
            Shape: shape,
            ShapeKnown: shapeKnown,
            Schema: config["schema"],
            Literal: hasLiteral ? config["literal"] : null,
            HasLiteral: hasLiteral,
            Binding: ConfigJson.String(config, "binding"),
            Prompt: ConfigJson.String(config, "prompt"),
            Default: hasDefault ? config["default"] : null,
            HasDefault: hasDefault,
            Resolved: hasResolved ? config[ResolvedKey] : null,
            HasResolved: hasResolved);
    }

    public const string ShapesSupported = "one, list";

    public static bool TryParseShape(string? value, out ControlValueShape shape)
    {
        switch (value?.Trim().ToLowerInvariant())
        {
            case "one": shape = ControlValueShape.Single; return true;
            case "list": shape = ControlValueShape.List; return true;
            default: shape = ControlValueShape.Single; return false;
        }
    }

    /// <summary>
    /// The value without asking anyone: the pre-pass's constant, else a literal, else a default. Null
    /// (with false) means this node still needs the binding store or a resolver.
    /// </summary>
    public bool TryGetStaticValue(out JsonNode? value)
    {
        if (HasResolved) { value = Resolved; return true; }
        if (HasLiteral) { value = Literal; return true; }
        if (HasDefault) { value = Default; return true; }

        value = null;
        return false;
    }
}
