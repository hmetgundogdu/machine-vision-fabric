using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Sdk;

public abstract class FrameSourceModuleBase<TOptions> : IFrameSourceModule
    where TOptions : class, new()
{
    private IntegrationModuleDescriptor? descriptor;

    public IntegrationModuleDescriptor Describe()
    {
        return descriptor ??= BuildDescriptor();
    }

    public IFrameSourceSession OpenSession(JsonElement configuration, string packageRoot)
    {
        return OpenSession(JsonConfigurationParser.Parse<TOptions>(configuration), packageRoot);
    }

    protected abstract IntegrationModuleDescriptor BuildDescriptor();

    protected abstract IFrameSourceSession OpenSession(TOptions options, string packageRoot);
}
