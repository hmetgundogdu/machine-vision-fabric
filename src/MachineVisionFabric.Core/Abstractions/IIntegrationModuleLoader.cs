using MachineVisionFabric.Contracts.Integrations;

namespace MachineVisionFabric.Core.Abstractions;

public interface IIntegrationModuleLoader
{
    IReadOnlyList<IIntegrationModule> LoadModules(string pluginRoot);
}
