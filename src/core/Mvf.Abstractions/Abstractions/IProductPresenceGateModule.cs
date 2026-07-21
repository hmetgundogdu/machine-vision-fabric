using System.Text.Json;
using Mvf.Graph.Integrations;

namespace Mvf.Abstractions;

public interface IProductPresenceGateModule : IIntegrationModule
{
    IProductPresenceGate CreateGate(JsonElement configuration);
}
