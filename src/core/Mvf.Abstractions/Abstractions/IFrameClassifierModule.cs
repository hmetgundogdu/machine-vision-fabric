using System.Text.Json;
using Mvf.Graph.Integrations;

namespace Mvf.Abstractions;

public interface IFrameClassifierModule : IIntegrationModule
{
    IFrameClassifier CreateClassifier(JsonElement configuration);
}
