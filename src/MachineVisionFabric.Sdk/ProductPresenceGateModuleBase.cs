using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Sdk;

public abstract class ProductPresenceGateModuleBase<TOptions> : IProductPresenceGateModule
    where TOptions : class, new()
{
    private IntegrationModuleDescriptor? descriptor;

    public IntegrationModuleDescriptor Describe()
    {
        return descriptor ??= BuildDescriptor();
    }

    public IProductPresenceGate CreateGate(JsonElement configuration)
    {
        return CreateGate(JsonConfigurationParser.Parse<TOptions>(configuration));
    }

    protected abstract IntegrationModuleDescriptor BuildDescriptor();

    protected abstract IProductPresenceGate CreateGate(TOptions options);
}
