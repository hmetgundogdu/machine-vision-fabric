using System.Text.Json;
using Mvf.Graph.Integrations;

namespace Mvf.Abstractions;

public interface IFrameAnalyzerModule : IIntegrationModule
{
    IFrameAnalyzer CreateAnalyzer(JsonElement configuration);
}
