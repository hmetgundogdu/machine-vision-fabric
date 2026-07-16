using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;

namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameSourceModule : IIntegrationModule
{
    IFrameSourceSession OpenSession(JsonElement configuration, string packageRoot);
}
