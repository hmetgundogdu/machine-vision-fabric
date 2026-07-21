using System.Text.Json;
using Mvf.Graph.Integrations;

namespace Mvf.Abstractions;

public interface IFrameProcessorModule : IIntegrationModule
{
    IFrameProcessor CreateProcessor(JsonElement configuration);
}
