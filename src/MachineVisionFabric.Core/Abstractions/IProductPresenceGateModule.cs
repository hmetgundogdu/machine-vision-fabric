using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;

namespace MachineVisionFabric.Core.Abstractions;

public interface IProductPresenceGateModule : IIntegrationModule
{
    IProductPresenceGate CreateGate(JsonElement configuration);
}
