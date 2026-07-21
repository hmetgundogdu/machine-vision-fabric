using System.Text.Json;
using Mvf.Graph.Integrations;
using Mvf.Abstractions;

namespace Mvf.Sdk;

/// <summary>
/// Convenience base class for <see cref="IFrameSinkModule"/> implementations.
/// Handles descriptor caching and typed config deserialization.
/// </summary>
public abstract class FrameSinkModuleBase<TOptions> : IFrameSinkModule
    where TOptions : class, new()
{
    private IntegrationModuleDescriptor? descriptor;

    public IntegrationModuleDescriptor Describe()
    {
        return descriptor ??= BuildDescriptor();
    }

    public IFrameSink OpenSink(JsonElement configuration, string packageRoot)
    {
        return OpenSink(JsonConfigurationParser.Parse<TOptions>(configuration), packageRoot);
    }

    protected abstract IntegrationModuleDescriptor BuildDescriptor();

    protected abstract IFrameSink OpenSink(TOptions options, string packageRoot);
}
