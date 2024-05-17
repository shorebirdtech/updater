import 'dart:io';

import 'package:mason_logger/mason_logger.dart';
import 'package:updater_tools/src/artifact_type.dart';
import 'package:updater_tools/src/commands/updater_tool_command.dart';
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

/// Function signature for the [PatchPackager] constructor.
typedef MakePatchPackager = PatchPackager Function({
  required File patchExecutable,
});

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
    final releaseFile = File(results[releaseCliArg] as String);
    final patchFile = File(results[patchCliArg] as String);
    final patchExecutable = File(results[patchExecutableCliArg] as String);
    final outputDirectory = Directory(results[outputCliArg] as String);
    final archiveType = ArchiveType.values.byName(
      results[archiveTypeCliArg] as String,
    );

    try {
      _assertCliArgsValid();
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

  /// Verifies that CLI arguments point to existing files. Throws an
  /// [ArgumentError] if any of the args are not valid.
  void _assertCliArgsValid() {
    final releaseFilePath = results[releaseCliArg] as String;
    final patchFilePath = results[patchCliArg] as String;
    final patchExecutablePath = results[patchExecutableCliArg] as String;

    _verifyFileExists(releaseFilePath, releaseCliArg);
    _verifyFileExists(patchFilePath, patchCliArg);
    _verifyFileExists(patchExecutablePath, patchExecutableCliArg);
  }

  /// Throws an [ArgumentError] if a file at [path] does not exist.
  void _verifyFileExists(String path, String name) {
    if (!File(path).existsSync()) {
      throw ArgumentError.value(
        path,
        name,
        'The $name file does not exist',
      );
    }
  }
}
