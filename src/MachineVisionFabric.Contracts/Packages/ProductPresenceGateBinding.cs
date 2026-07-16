using System.Text.Json;

namespace MachineVisionFabric.Contracts.Packages;

public sealed class ProductPresenceGateBinding
{
    public string Mode { get; set; } = "builtin";

    public string? ModuleId { get; set; }

    public JsonElement Config { get; set; }
}
