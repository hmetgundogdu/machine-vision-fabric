using System.Text.Json;
using Mvf.Graph.Integrations;
using Mvf.Abstractions;

namespace Mvf.Sdk;

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
