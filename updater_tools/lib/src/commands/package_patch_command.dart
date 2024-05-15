import 'package:mason_logger/mason_logger.dart';
import 'package:updater_tools/src/artifact_type.dart';
import 'package:updater_tools/src/commands/updater_tool_command.dart';

/// {@template package_patch_command}
/// A command to package patch artifacts.
/// {@endtemplate}
class PackagePatchCommand extends UpdaterToolCommand {
  /// {@macro package_patch_command}
  PackagePatchCommand() {
    argParser
      ..addOption(
        'archive-type',
        help: 'The format of release and patch. These *must* be the same.',
        allowed: ArchiveType.values.asNameMap().keys,
        mandatory: true,
      )
      ..addOption(
        'release',
        abbr: 'r',
        mandatory: true,
        help: 'The path to the release artifact which will be patched',
      )
      ..addOption(
        'patch',
        abbr: 'p',
        mandatory: true,
        help: 'The path to the patch artifact which will be packaged',
      )
      ..addOption(
        'patch-executable',
        mandatory: true,
        help:
            '''The path to the patch executable that creates a binary diff between two files''',
      )
      ..addOption(
        'output',
        abbr: 'o',
        mandatory: true,
        help: '''
Where to write the packaged patch archives.

This should be a directory, and will contain patch archives for each architecture.''',
      );
  }

  @override
  String get description =>
      '''A command that turns two app archives (.aab, .xcarchive, etc.) into patch artifacts.''';

  @override
  String get name => 'package_patch';

  @override
  Future<int> run() async {
    final releaseFilePath = results['release'] as String;
    final patchFilePath = results['patch'] as String;
    final patchExecutablePath = results['patch-executable'] as String;
    final outDirectoryPath = results['output'] as String;
    final archiveType = ArchiveType.values.byName(
      results['archive-type'] as String,
    );

    return ExitCode.success.code;
  }
}
