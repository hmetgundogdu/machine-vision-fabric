using System.Text.Json;
using Mvf.Graph.Integrations;
using Mvf.Abstractions;

namespace Mvf.Sdk;

/// <summary>
/// Base class for integration modules that classify frame content into a control
/// signal (perception → control). Subclasses supply a descriptor and build the
/// classifier from strongly-typed options.
/// </summary>
public abstract class FrameClassifierModuleBase<TOptions> : IFrameClassifierModule
    where TOptions : class, new()
{
    public IntegrationModuleDescriptor Describe()
    {
        return BuildDescriptor();
    }

    public IFrameClassifier CreateClassifier(JsonElement configuration)
    {
        return CreateClassifier(JsonConfigurationParser.Parse<TOptions>(configuration));
    }

    protected abstract IntegrationModuleDescriptor BuildDescriptor();

    protected abstract IFrameClassifier CreateClassifier(TOptions options);
}
