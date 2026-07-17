using System.Text.Json;

namespace MachineVisionFabric.Contracts.Packages;

public sealed class FrameProcessorBinding
{
    public string Mode { get; set; } = "none";

    public string? ModuleId { get; set; }

    public JsonElement Config { get; set; }
}
