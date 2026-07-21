using System.Text.Json;
using Mvf.Graph.Integrations;

namespace Mvf.Abstractions;

public interface IFrameSourceModule : IIntegrationModule
{
    IFrameSourceSession OpenSession(JsonElement configuration, string packageRoot);
}
