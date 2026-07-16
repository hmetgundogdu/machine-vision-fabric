using System.Text.Json;

namespace MachineVisionFabric.Contracts.Packages;

public sealed class SourceBinding
{
    public string Mode { get; set; } = "builtin";

    public string? ModuleId { get; set; }

    public JsonElement Config { get; set; }
}
