using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace Mvf.Graph.Values;

/// <summary>
/// A deliberately small JSON Schema subset, enough to keep a <c>json</c> control value honest without
/// pulling a schema engine into the core. Supported keywords:
/// <c>type</c>, <c>enum</c>, <c>const</c>, <c>properties</c>, <c>required</c>,
/// <c>additionalProperties</c>, <c>items</c>, <c>minItems</c>/<c>maxItems</c>,
/// <c>minimum</c>/<c>maximum</c>/<c>exclusiveMinimum</c>/<c>exclusiveMaximum</c>,
/// <c>minLength</c>/<c>maxLength</c>, <c>pattern</c>.
///
/// <para>Unknown keywords are ignored, which is what the specification requires of a validator — so a
/// schema using <c>$ref</c> or <c>allOf</c> is <b>accepted and under-enforced</b>, never rejected. If
/// enforcement has to get stricter than this, swap the implementation behind these two methods rather
/// than spreading schema knowledge through the engine.</para>
/// </summary>
public static class JsonSchemaCheck
{
    private static readonly string[] KnownTypes =
        ["object", "array", "string", "number", "integer", "boolean", "null"];

    /// <summary>
    /// Structural check on the schema document itself — the graph is rejected at validation time rather
    /// than failing on the first value that reaches it.
    /// </summary>
    public static bool TryValidateSchema(JsonNode? schema, out string? error)
    {
        error = null;

        if (schema is null)
        {
            return true;
        }

        // A boolean schema (true/false) is legal JSON Schema and needs no further checking.
        if (schema is JsonValue boolSchema && boolSchema.TryGetValue<bool>(out _))
        {
            return true;
        }

        if (schema is not JsonObject obj)
        {
            error = "a schema must be a JSON object (or a boolean)";
            return false;
        }

        if (obj.TryGetPropertyValue("type", out var typeNode) && typeNode is not null)
        {
            List<string?> types = typeNode is JsonArray array
                ? array.Select(AsString).ToList()
                : [AsString(typeNode)];

            foreach (var declared in types)
            {
                if (declared is null || !KnownTypes.Contains(declared, StringComparer.Ordinal))
                {
                    error = $"'type' must be one of {string.Join(", ", KnownTypes)} but was '{declared ?? "null"}'";
                    return false;
                }
            }
        }

        if (obj.TryGetPropertyValue("required", out var required) && required is not null && required is not JsonArray)
        {
            error = "'required' must be an array of property names";
            return false;
        }

        if (obj.TryGetPropertyValue("enum", out var enumNode) && enumNode is not null && enumNode is not JsonArray)
        {
            error = "'enum' must be an array";
            return false;
        }

        if (obj.TryGetPropertyValue("pattern", out var pattern) && pattern is not null)
        {
            if (pattern is not JsonValue patternValue || !patternValue.TryGetValue<string>(out var patternText))
            {
                error = "'pattern' must be a string";
                return false;
            }

            try
            {
                _ = new Regex(patternText);
            }
            catch (ArgumentException ex)
            {
                error = $"'pattern' is not a valid regular expression: {ex.Message}";
                return false;
            }
        }

        if (obj.TryGetPropertyValue("properties", out var properties) && properties is not null)
        {
            if (properties is not JsonObject propertyObject)
            {
                error = "'properties' must be an object";
                return false;
            }

            foreach (var (name, subSchema) in propertyObject)
            {
                if (!TryValidateSchema(subSchema, out var subError))
                {
                    error = $"property '{name}': {subError}";
                    return false;
                }
            }
        }

        if (obj.TryGetPropertyValue("items", out var items) && items is not null && !TryValidateSchema(items, out var itemsError))
        {
            error = $"'items': {itemsError}";
            return false;
        }

        return true;
    }

    /// <summary>Validates a value against a schema. A null schema accepts everything.</summary>
    public static bool TryValidate(JsonNode? schema, JsonNode? value, out string? error) =>
        Validate(schema, value, "$", out error);

    /// <summary>
    /// Validates a value of a declared shape. For a list the schema describes an <b>element</b>, so it is
    /// applied to each in turn — the same schema then reads the same whether a record arrives alone or in
    /// bulk, which is the only reading that survives discovery publishing a collection of what a `value`
    /// node publishes one of.
    ///
    /// <para>Every place a value enters the graph goes through here — a config literal, an operator's
    /// answer, a stored binding, a live tuning edit. Keeping it in one function is the point: the
    /// list-versus-element distinction was got wrong independently in three of those four places.</para>
    /// </summary>
    public static bool TryValidateShaped(
        ControlValueShape shape,
        JsonNode? schema,
        JsonNode? value,
        out string? error)
    {
        error = null;

        if (schema is null)
        {
            return true;
        }

        if (shape == ControlValueShape.Single)
        {
            return Validate(schema, value, "$", out error);
        }

        if (value is not JsonArray items)
        {
            // A shape mismatch is a type error, reported by the caller with a better message than a
            // schema violation would give.
            return true;
        }

        for (var i = 0; i < items.Count; i++)
        {
            if (!Validate(schema, items[i], $"$[{i}]", out error))
            {
                return false;
            }
        }

        return true;
    }

    private static bool Validate(JsonNode? schema, JsonNode? value, string path, out string? error)
    {
        error = null;

        if (schema is null)
        {
            return true;
        }

        if (schema is JsonValue boolSchema && boolSchema.TryGetValue<bool>(out var accepts))
        {
            if (accepts)
            {
                return true;
            }

            error = $"{path}: schema is 'false', which accepts nothing";
            return false;
        }

        if (schema is not JsonObject obj)
        {
            return true;
        }

        var kind = KindOf(value);

        if (obj.TryGetPropertyValue("type", out var typeNode) && typeNode is not null)
        {
            List<string?> declaredTypes = typeNode is JsonArray typeArray
                ? typeArray.Select(AsString).ToList()
                : [AsString(typeNode)];
            var types = declaredTypes.Where(s => s is not null).Select(s => s!).ToList();

            if (!types.Any(t => MatchesType(t, kind, value)))
            {
                error = $"{path}: expected {string.Join(" or ", types)} but found {kind}";
                return false;
            }
        }

        if (obj.TryGetPropertyValue("const", out var constNode)
            && !JsonNode.DeepEquals(constNode, value))
        {
            error = $"{path}: must be {constNode?.ToJsonString() ?? "null"}";
            return false;
        }

        if (obj.TryGetPropertyValue("enum", out var enumNode) && enumNode is JsonArray allowed
            && !allowed.Any(candidate => JsonNode.DeepEquals(candidate, value)))
        {
            error = $"{path}: must be one of {string.Join(", ", allowed.Select(a => a?.ToJsonString() ?? "null"))}";
            return false;
        }

        return kind switch
        {
            "object" => ValidateObject(obj, (JsonObject)value!, path, out error),
            "array" => ValidateArray(obj, (JsonArray)value!, path, out error),
            "string" => ValidateString(obj, AsString(value) ?? string.Empty, path, out error),
            "number" or "integer" => ValidateNumber(obj, value!, path, out error),
            _ => true
        };
    }

    private static bool ValidateObject(JsonObject schema, JsonObject value, string path, out string? error)
    {
        error = null;

        if (schema.TryGetPropertyValue("required", out var requiredNode) && requiredNode is JsonArray required)
        {
            foreach (var nameNode in required)
            {
                var name = nameNode is JsonValue v && v.TryGetValue<string>(out var s) ? s : null;
                if (name is not null && !value.ContainsKey(name))
                {
                    error = $"{path}: missing required property '{name}'";
                    return false;
                }
            }
        }

        var properties = schema["properties"] as JsonObject;
        if (properties is not null)
        {
            foreach (var (name, subSchema) in properties)
            {
                if (value.TryGetPropertyValue(name, out var propertyValue)
                    && !Validate(subSchema, propertyValue, $"{path}.{name}", out error))
                {
                    return false;
                }
            }
        }

        if (schema.TryGetPropertyValue("additionalProperties", out var additional) && additional is not null)
        {
            var declared = properties?.Select(p => p.Key).ToHashSet(StringComparer.Ordinal) ?? [];
            var forbidden = additional is JsonValue av && av.TryGetValue<bool>(out var allow) && !allow;

            foreach (var (name, propertyValue) in value)
            {
                if (declared.Contains(name))
                {
                    continue;
                }

                if (forbidden)
                {
                    error = $"{path}: property '{name}' is not allowed";
                    return false;
                }

                if (!forbidden && additional is JsonObject && !Validate(additional, propertyValue, $"{path}.{name}", out error))
                {
                    return false;
                }
            }
        }

        return true;
    }

    private static bool ValidateArray(JsonObject schema, JsonArray value, string path, out string? error)
    {
        error = null;

        if (TryGetInt(schema, "minItems", out var min) && value.Count < min)
        {
            error = $"{path}: needs at least {min} item(s) but has {value.Count}";
            return false;
        }

        if (TryGetInt(schema, "maxItems", out var max) && value.Count > max)
        {
            error = $"{path}: allows at most {max} item(s) but has {value.Count}";
            return false;
        }

        if (schema.TryGetPropertyValue("items", out var items) && items is not null)
        {
            for (var i = 0; i < value.Count; i++)
            {
                if (!Validate(items, value[i], $"{path}[{i}]", out error))
                {
                    return false;
                }
            }
        }

        return true;
    }

    private static bool ValidateString(JsonObject schema, string value, string path, out string? error)
    {
        error = null;

        if (TryGetInt(schema, "minLength", out var min) && value.Length < min)
        {
            error = $"{path}: must be at least {min} character(s)";
            return false;
        }

        if (TryGetInt(schema, "maxLength", out var max) && value.Length > max)
        {
            error = $"{path}: must be at most {max} character(s)";
            return false;
        }

        if (schema["pattern"] is JsonValue patternValue
            && patternValue.TryGetValue<string>(out var pattern)
            && !Regex.IsMatch(value, pattern))
        {
            error = $"{path}: does not match pattern '{pattern}'";
            return false;
        }

        return true;
    }

    private static bool ValidateNumber(JsonObject schema, JsonNode value, string path, out string? error)
    {
        error = null;

        // Via the writer again: a JsonValue built from a long does not answer GetValue<double>.
        double number;
        try
        {
            using var document = JsonDocument.Parse(value.ToJsonString());
            number = document.RootElement.GetDouble();
        }
        catch (Exception ex) when (ex is JsonException or FormatException)
        {
            return true;
        }

        if (TryGetDouble(schema, "minimum", out var min) && number < min)
        {
            error = $"{path}: must be >= {min}";
            return false;
        }

        if (TryGetDouble(schema, "maximum", out var max) && number > max)
        {
            error = $"{path}: must be <= {max}";
            return false;
        }

        if (TryGetDouble(schema, "exclusiveMinimum", out var exclusiveMin) && number <= exclusiveMin)
        {
            error = $"{path}: must be > {exclusiveMin}";
            return false;
        }

        if (TryGetDouble(schema, "exclusiveMaximum", out var exclusiveMax) && number >= exclusiveMax)
        {
            error = $"{path}: must be < {exclusiveMax}";
            return false;
        }

        return true;
    }

    private static bool MatchesType(string declared, string actual, JsonNode? value) => declared switch
    {
        "integer" => actual == "integer" || (actual == "number" && IsIntegral(value)),
        "number" => actual is "number" or "integer",
        _ => declared == actual
    };

    private static bool IsIntegral(JsonNode? value)
    {
        if (value is null)
        {
            return false;
        }

        try
        {
            using var document = JsonDocument.Parse(value.ToJsonString());
            return document.RootElement.ValueKind == JsonValueKind.Number
                && document.RootElement.TryGetInt64(out _);
        }
        catch (JsonException)
        {
            return false;
        }
    }

    private static string KindOf(JsonNode? node)
    {
        switch (node)
        {
            case null:
                return "null";
            case JsonObject:
                return "object";
            case JsonArray:
                return "array";
        }

        try
        {
            using var document = JsonDocument.Parse(node.ToJsonString());
            return document.RootElement.ValueKind switch
            {
                JsonValueKind.String => "string",
                JsonValueKind.True or JsonValueKind.False => "boolean",
                JsonValueKind.Number => document.RootElement.TryGetInt64(out _) ? "integer" : "number",
                JsonValueKind.Null => "null",
                _ => "unknown"
            };
        }
        catch (JsonException)
        {
            return "unknown";
        }
    }

    private static string? AsString(JsonNode? node) =>
        node is JsonValue value && value.TryGetValue<string>(out var text) ? text : null;

    // Both read through the writer for the same reason Matches does: TryGetValue<double> answers false on
    // a JsonValue built from an int, so a hand-built schema like { "maximum": 255 } would silently stop
    // being enforced — a bound that is not applied is worse than no bound at all.
    private static bool TryGetInt(JsonObject schema, string keyword, out int result)
    {
        result = 0;

        if (!TryGetDouble(schema, keyword, out var number) || number is < int.MinValue or > int.MaxValue)
        {
            return false;
        }

        result = (int)number;
        return true;
    }

    private static bool TryGetDouble(JsonObject schema, string keyword, out double result)
    {
        result = 0;

        if (schema[keyword] is not JsonValue value)
        {
            return false;
        }

        try
        {
            using var document = JsonDocument.Parse(value.ToJsonString());
            return document.RootElement.ValueKind == JsonValueKind.Number
                && document.RootElement.TryGetDouble(out result);
        }
        catch (JsonException)
        {
            return false;
        }
    }
}
