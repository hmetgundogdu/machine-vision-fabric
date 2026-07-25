using System.Text.Json.Nodes;
using Mvf.Graph.Values;

namespace Mvf.Engine.Values;

/// <summary>
/// One live-editable config field of a module node, declared in the node's <c>bindings</c> map. The
/// field's current value lives in <c>config</c>; this record only carries how to <b>type-check</b> an edit
/// (its <see cref="Type"/> and optional <see cref="Schema"/>) and where to <b>persist</b> it
/// (<see cref="Binding"/>, or null for a run-only tunable). A change re-activates the node.
/// </summary>
public sealed record ModuleBinding(
    string Field,
    string RawType,
    ControlValueType Type,
    bool TypeKnown,
    JsonNode? Schema,
    string? Binding,
    string? Prompt)
{
    /// <summary>The registry key a module binding is tuned under — a field scoped to its node.</summary>
    public string LiveKey(string nodeId) => $"{nodeId}.{Field}";
}

public static class ModuleBindings
{
    public static IReadOnlyList<ModuleBinding> Read(JsonObject bindings)
    {
        ArgumentNullException.ThrowIfNull(bindings);

        var result = new List<ModuleBinding>(bindings.Count);
        foreach (var (field, spec) in bindings)
        {
            if (spec is not JsonObject obj)
            {
                // A malformed entry is carried as an unknown-typed binding so the validator names the field
                // rather than the expander throwing — the same "carry, then judge" split as elsewhere.
                result.Add(new ModuleBinding(field, "(not an object)", ControlValueType.Json, false, null, null, null));
                continue;
            }

            var rawType = ConfigJson.String(obj, "type") ?? "string";
            var typeKnown = ControlValueTypes.TryParse(rawType, out var type);

            result.Add(new ModuleBinding(
                Field: field,
                RawType: rawType,
                Type: type,
                TypeKnown: typeKnown,
                Schema: obj["schema"],
                Binding: ConfigJson.String(obj, "binding"),
                Prompt: ConfigJson.String(obj, "prompt")));
        }

        return result;
    }
}
