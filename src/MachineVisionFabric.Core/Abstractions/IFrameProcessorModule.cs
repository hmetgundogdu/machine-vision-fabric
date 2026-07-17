using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;

namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameProcessorModule : IIntegrationModule
{
    IFrameProcessor CreateProcessor(JsonElement configuration);
}
