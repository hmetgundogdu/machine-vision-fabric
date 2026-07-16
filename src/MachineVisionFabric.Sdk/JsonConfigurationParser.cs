using System.Text.Json;

namespace MachineVisionFabric.Sdk;

public static class JsonConfigurationParser
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public static TOptions Parse<TOptions>(JsonElement configuration) where TOptions : class, new()
    {
        return configuration.ValueKind is JsonValueKind.Undefined or JsonValueKind.Null
            ? new TOptions()
            : JsonSerializer.Deserialize<TOptions>(configuration.GetRawText(), JsonOptions) ?? new TOptions();
    }
}
