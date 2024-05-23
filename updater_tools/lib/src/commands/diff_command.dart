import 'dart:io';

import 'package:mason_logger/mason_logger.dart';
import 'package:updater_tools/src/commands/updater_tool_command.dart';
import 'package:updater_tools/src/extensions/arg_results.dart';
import 'package:updater_tools/src/logger.dart';
import 'package:updater_tools/src/packager/patch_packager.dart';

/// The arg name to specify the path to the release binary.
const releaseCliArg = 'release';

/// The arg name to specify the path to the patch binary.
const patchCliArg = 'patch';

/// The arg name to specify the path to the patch executable.
const patchExecutableCliArg = 'patch-executable';

/// The arg name to specify the output file.
const outputCliArg = 'output';

/// {@template diff_command}
/// A wrapper around the patch executable
/// {@endtemplate}
class DiffCommand extends UpdaterToolCommand {
  /// {@macro diff_command}
  DiffCommand([MakePatchPackager? makePatchPackager])
      : _makePatchPackagerOverride = makePatchPackager,
        super() {
    argParser
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
  String get name => 'diff';

  @override
  String get description =>
      '''Outputs a binary diff of the provided release and patch files, using release as a base.''';

  @override
  Future<int> run() async {
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

    final patchPackager = (_makePatchPackagerOverride ?? PatchPackager.new)(
      patchExecutable: patchExecutable,
    );
    await patchPackager.makeDiff(
      base: releaseFile,
      patch: patchFile,
      outFile: File(results[outputCliArg] as String),
    );

    return ExitCode.success.code;
  }
}
