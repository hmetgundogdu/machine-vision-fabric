using System.Text.Json;
using Mvf.Graph.Integrations;
using Mvf.Abstractions;

namespace Mvf.Sdk;

public abstract class FrameProcessorModuleBase<TOptions> : IFrameProcessorModule
    where TOptions : class, new()
{
    public IntegrationModuleDescriptor Describe()
    {
        return BuildDescriptor();
    }

    public IFrameProcessor CreateProcessor(JsonElement configuration)
    {
        return CreateProcessor(JsonConfigurationParser.Parse<TOptions>(configuration));
    }

    protected abstract IntegrationModuleDescriptor BuildDescriptor();

    protected abstract IFrameProcessor CreateProcessor(TOptions options);
}
