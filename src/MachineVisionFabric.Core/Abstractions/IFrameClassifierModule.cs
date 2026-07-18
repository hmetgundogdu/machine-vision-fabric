using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;

namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameClassifierModule : IIntegrationModule
{
    IFrameClassifier CreateClassifier(JsonElement configuration);
}
