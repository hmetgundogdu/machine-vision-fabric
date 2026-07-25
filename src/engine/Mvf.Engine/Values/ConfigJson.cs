using System.Text.Json.Nodes;

namespace Mvf.Engine.Values;

/// <summary>Small readers shared by the value/select config records.</summary>
internal static class ConfigJson
{
    public static string? String(JsonObject config, string property) =>
        config[property] is JsonValue value && value.TryGetValue<string>(out var text) && text.Length > 0
            ? text
            : null;
}
