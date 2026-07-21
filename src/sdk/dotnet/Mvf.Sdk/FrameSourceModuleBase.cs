using System.Text.Json;
using Mvf.Graph.Integrations;
using Mvf.Abstractions;

namespace Mvf.Sdk;

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
