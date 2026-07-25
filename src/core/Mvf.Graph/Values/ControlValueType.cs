using System.Text.Json;
using System.Text.Json.Nodes;

namespace Mvf.Graph.Values;

/// <summary>
/// The element type of a control value — a value the graph cannot compute (a threshold, an output
/// folder, a camera record). Deliberately a small closed set rather than a type system: anything
/// richer travels as <see cref="Json"/> and declares a JSON Schema, which is what keeps "the camera
/// record" typed without the core knowing what a camera is.
/// </summary>
public enum ControlValueType
{
    String,
    Int,
    Number,
    Bool,
    Json
}

public static class ControlValueTypes
{
    public const string Supported = "string, int, number, bool, json";

    public static bool TryParse(string? value, out ControlValueType type)
    {
        switch (value?.Trim().ToLowerInvariant())
        {
            case "string": type = ControlValueType.String; return true;
            case "int": type = ControlValueType.Int; return true;
            case "number": type = ControlValueType.Number; return true;
            case "bool": type = ControlValueType.Bool; return true;
            case "json": type = ControlValueType.Json; return true;
            default: type = ControlValueType.Json; return false;
        }
    }

    public static string ToToken(ControlValueType type) => type switch
    {
        ControlValueType.String => "string",
        ControlValueType.Int => "int",
        ControlValueType.Number => "number",
        ControlValueType.Bool => "bool",
        _ => "json"
    };

    /// <summary>
    /// Does this node carry a value of the declared type? Checked wherever a value enters the graph —
    /// a config literal, an operator's answer, a stored binding, or items arriving on an edge.
    /// </summary>
    public static bool Matches(ControlValueType type, JsonNode? node)
    {
        if (type == ControlValueType.Json)
        {
            return node is not null;
        }

        if (node is not JsonValue)
        {
            return false;
        }

        // Normalising through the writer is what makes this work for both a parsed node (a JsonElement
        // wrapper) and one built by hand with JsonValue.Create — the two do not agree on TryGetValue.
        JsonValueKind kind;
        bool integral;
        try
        {
            using var document = JsonDocument.Parse(node.ToJsonString());
            kind = document.RootElement.ValueKind;
            integral = kind == JsonValueKind.Number && document.RootElement.TryGetInt64(out _);
        }
        catch (JsonException)
        {
            return false;
        }

        return type switch
        {
            ControlValueType.String => kind == JsonValueKind.String,
            ControlValueType.Bool => kind is JsonValueKind.True or JsonValueKind.False,
            ControlValueType.Int => integral,
            ControlValueType.Number => kind == JsonValueKind.Number,
            _ => false
        };
    }

    /// <summary>
    /// Does this node carry a value of the declared type <b>and shape</b>? A list must be a JSON array
    /// whose every element matches — a heterogeneous array is not a <c>control/list:&lt;t&gt;</c>.
    /// </summary>
    public static bool MatchesShape(ControlValueShape shape, ControlValueType type, JsonNode? node)
    {
        if (shape == ControlValueShape.Single)
        {
            return Matches(type, node);
        }

        return node is JsonArray array && array.All(element => Matches(type, element));
    }

    /// <summary>
    /// Turns text typed by an operator (or read from an environment variable) into a node of the
    /// declared type. Returns false when the text is not that type — "abc" for an int, say.
    /// </summary>
    public static bool TryParseValue(ControlValueType type, string text, out JsonNode? value)
    {
        value = null;

        switch (type)
        {
            case ControlValueType.String:
                value = JsonValue.Create(text);
                return true;

            case ControlValueType.Int:
                if (!long.TryParse(text.Trim(), out var i)) return false;
                value = JsonValue.Create(i);
                return true;

            case ControlValueType.Number:
                if (!double.TryParse(
                        text.Trim(),
                        System.Globalization.NumberStyles.Float,
                        System.Globalization.CultureInfo.InvariantCulture,
                        out var d)) return false;
                value = JsonValue.Create(d);
                return true;

            case ControlValueType.Bool:
                if (!bool.TryParse(text.Trim(), out var b)) return false;
                value = JsonValue.Create(b);
                return true;

            default:
                try
                {
                    value = JsonNode.Parse(text);
                    return value is not null;
                }
                catch (JsonException)
                {
                    return false;
                }
        }
    }

    /// <summary>
    /// <see cref="TryParseValue"/> for either shape. A list is always written as JSON, because there is no
    /// unambiguous plain-text spelling of a collection — and inventing a comma convention here would put a
    /// second, undeclared syntax into the one place values enter the graph.
    /// </summary>
    public static bool TryParseShaped(
        ControlValueShape shape,
        ControlValueType type,
        string text,
        out JsonNode? value)
    {
        if (shape == ControlValueShape.Single)
        {
            return TryParseValue(type, text, out value);
        }

        try
        {
            value = JsonNode.Parse(text);
        }
        catch (JsonException)
        {
            value = null;
            return false;
        }

        return MatchesShape(shape, type, value);
    }
}
