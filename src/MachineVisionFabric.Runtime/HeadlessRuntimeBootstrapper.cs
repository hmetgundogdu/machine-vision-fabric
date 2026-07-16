using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Runtime;
using MachineVisionFabric.Core.Abstractions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;

namespace MachineVisionFabric.Runtime;

public sealed class HeadlessRuntimeBootstrapper(
    IOptions<MachineVisionFabricRuntimeOptions> options,
    IPackageManifestLoader packageManifestLoader,
    IEntryProfileLoader entryProfileLoader,
    IDatasetSessionPreparer datasetSessionPreparer,
    IFrameSourceResolver frameSourceResolver,
    IProductPresenceGateResolver productPresenceGateResolver,
    IDatasetCollector datasetCollector,
    ILogger<HeadlessRuntimeBootstrapper> logger) : IHeadlessRuntimeBootstrapper
{
    public async Task<HeadlessBootstrapReport> BootstrapAsync(CancellationToken cancellationToken)
    {
        var runtimeOptions = options.Value;
        var packageRoot = ResolveAgainstAppBase(runtimeOptions.DatasetCapture.PackageRoot);
        var datasetRoot = ResolveAgainstAppBase(runtimeOptions.DatasetCapture.DatasetRoot);
        var integrationsRoot = ResolveAgainstAppBase(runtimeOptions.IntegrationsRoot);

        logger.LogInformation(
            "Resolved runtime paths. PackageRoot={PackageRoot}; DatasetRoot={DatasetRoot}; IntegrationsRoot={IntegrationsRoot}",
            packageRoot,
            datasetRoot,
            integrationsRoot);

        var manifest = await packageManifestLoader.LoadAsync(packageRoot, cancellationToken);
        var profile = await entryProfileLoader.LoadAsync(packageRoot, manifest.EntryProfile, cancellationToken);

        foreach (var requiredDirectory in manifest.RequiredDirectories)
        {
            var fullRequiredDirectory = Path.Combine(packageRoot, requiredDirectory);
            Directory.CreateDirectory(fullRequiredDirectory);
        }

        Directory.CreateDirectory(datasetRoot);

        var sessionRoot = datasetSessionPreparer.PrepareSessionRoot(
            datasetRoot,
            runtimeOptions.DatasetCapture.SessionPrefix,
            runtimeOptions.DatasetCapture.CreateSessionOnStartup);

        var gateResolution = productPresenceGateResolver.Resolve(manifest, integrationsRoot);
        var frameSourceResolution = frameSourceResolver.Resolve(profile, packageRoot, integrationsRoot);
        await using var frameSourceSession = frameSourceResolution.Session;
        var estimatedFrameCount = frameSourceSession.EstimatedFrameCount;
        var datasetCollection = await datasetCollector.CollectAsync(
            sessionRoot,
            manifest,
            frameSourceSession.DeclaredCameraCount,
            gateResolution.Gate,
            frameSourceSession,
            cancellationToken);

        logger.LogInformation(
            "Dataset-first bootstrap prepared package '{PackageName}' with profile '{ProfileName}' at {PackageRoot}. Session root: {SessionRoot}. Source strategy: {SourceStrategy}. Source: {Source}. Estimated frames: {FrameCount}. Product present: {ProductPresent}. Gate strategy: {GateStrategy}. Captured frames: {CapturedFrameCount}.",
            manifest.Name,
            profile.Name,
            packageRoot,
            sessionRoot,
            frameSourceResolution.Strategy,
            frameSourceResolution.Source,
            estimatedFrameCount,
            datasetCollection.ProductPresenceDecision.ProductPresent,
            gateResolution.Strategy,
            datasetCollection.CapturedFrameCount);

        return new HeadlessBootstrapReport(
            packageRoot,
            sessionRoot,
            runtimeOptions.DatasetCapture.CreateSessionOnStartup,
            estimatedFrameCount ?? datasetCollection.CapturedFrameCount,
            frameSourceSession.DeclaredCameraCount,
            datasetCollection.CapturedFrameCount,
            datasetCollection.SessionMetadataPath,
            datasetCollection.ProductPresenceDecision.ProductPresent,
            gateResolution.Source,
            gateResolution.Strategy,
            frameSourceResolution.Source,
            frameSourceResolution.Strategy);
    }

    private static string ResolveAgainstAppBase(string path)
    {
        var repositoryRoot = ResolveRepositoryRoot(typeof(HeadlessRuntimeBootstrapper).Assembly.Location);

        return Path.IsPathRooted(path)
            ? Path.GetFullPath(path)
            : Path.GetFullPath(Path.Combine(repositoryRoot, path));
    }

    private static string ResolveRepositoryRoot(string assemblyLocation)
    {
        var currentDirectory = new DirectoryInfo(Path.GetDirectoryName(assemblyLocation) ?? AppContext.BaseDirectory);
        while (currentDirectory is not null)
        {
            var hasSrc = Directory.Exists(Path.Combine(currentDirectory.FullName, "src"));
            var hasExamples = Directory.Exists(Path.Combine(currentDirectory.FullName, "examples"));
            var hasLegacySamples = Directory.Exists(Path.Combine(currentDirectory.FullName, "samples"));
            if (hasSrc && (hasExamples || hasLegacySamples))
            {
                return currentDirectory.FullName;
            }

            currentDirectory = currentDirectory.Parent;
        }

        return Path.GetDirectoryName(assemblyLocation) ?? AppContext.BaseDirectory;
    }
}
