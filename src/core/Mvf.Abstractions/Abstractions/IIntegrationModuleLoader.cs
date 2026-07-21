using Mvf.Graph.Integrations;

namespace Mvf.Abstractions;

public interface IIntegrationModuleLoader
{
    IReadOnlyList<IIntegrationModule> LoadModules(string pluginRoot);
}
