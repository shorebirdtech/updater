import 'dart:io';

import 'package:mason_logger/mason_logger.dart';
import 'package:updater_tools/src/artifact_type.dart';
import 'package:updater_tools/src/commands/updater_tool_command.dart';
import 'package:updater_tools/src/extensions/arg_results.dart';
import 'package:updater_tools/src/logger.dart';
import 'package:updater_tools/src/packager/patch_packager.dart';

/// The arg name to specify the release and patch archive type.
const archiveTypeCliArg = 'archive-type';

/// The arg name to specify the path to the release archive.
const releaseCliArg = 'release';

/// The arg name to specify the path to the patch archive.
const patchCliArg = 'patch';

/// The arg name to specify the path to the patch executable.
const patchExecutableCliArg = 'patch-executable';

/// The arg name to specify the output directory.
const outputCliArg = 'output';

/// {@template package_patch_command}
/// A command to package patch artifacts.
/// {@endtemplate}
class PackagePatchCommand extends UpdaterToolCommand {
  /// {@macro package_patch_command}
  PackagePatchCommand([MakePatchPackager? makePatchPackager])
      : _makePatchPackagerOverride = makePatchPackager,
        super() {
    argParser
      ..addOption(
        archiveTypeCliArg,
        help: 'The format of release and patch. These *must* be the same.',
        allowed: ArchiveType.values.asNameMap().keys,
        mandatory: true,
      )
      ..addOption(
        releaseCliArg,
        abbr: 'r',
        mandatory: true,
        help: 'The path to the release artifact which will be patched',
      )
      ..addOption(
        patchCliArg,
        abbr: 'p',
        mandatory: true,
        help: 'The path to the patch artifact which will be packaged',
      )
      ..addOption(
        patchExecutableCliArg,
        mandatory: true,
        help:
            '''The path to the patch executable that creates a binary diff between two files''',
      )
      ..addOption(
        outputCliArg,
        abbr: 'o',
        mandatory: true,
        help: '''
Where to write the packaged patch archives.

This should be a directory, and will contain patch archives for each architecture.''',
      );
  }

  final MakePatchPackager? _makePatchPackagerOverride;

  @override
  String get description =>
      '''A command that turns two app archives (.aab, .xcarchive, etc.) into patch artifacts.''';

  @override
  String get name => 'package_patch';

  @override
  Future<int> run() async {
    final outputDirectory = Directory(results[outputCliArg] as String);
    final archiveType = ArchiveType.values.byName(
      results[archiveTypeCliArg] as String,
    );

    final File releaseFile;
    final File patchFile;
    final File patchExecutable;
    try {
      releaseFile = results.asExistingFile(releaseCliArg);
      patchFile = results.asExistingFile(patchCliArg);
      patchExecutable = results.asExistingFile(patchExecutableCliArg);
    } catch (e) {
      logger.err('$e');
      return ExitCode.usage.code;
    }

    if (outputDirectory.existsSync()) {
      logger.info('${outputDirectory.path} already exists. Deleting...');
      outputDirectory.deleteSync(recursive: true);
    }

    final packager = (_makePatchPackagerOverride ?? PatchPackager.new)(
      patchExecutable: patchExecutable,
    );
    await packager.packagePatch(
      releaseArchive: releaseFile,
      patchArchive: patchFile,
      archiveType: archiveType,
      outputDirectory: outputDirectory,
    );

    logger.info('Patch packaged to ${outputDirectory.path}');

    return ExitCode.success.code;
  }
}
