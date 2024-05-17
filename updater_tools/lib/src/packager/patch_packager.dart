import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:mason_logger/mason_logger.dart';
import 'package:path/path.dart' as p;
import 'package:updater_tools/src/artifact_type.dart';
import 'package:updater_tools/src/extensions/extensions.dart';
import 'package:updater_tools/src/logger.dart';
import 'package:updater_tools/src/process.dart';

/// {@template packaging_exception}
/// An exception thrown when creating a patch package fails.
/// {@endtemplate}
class PackagingException implements Exception {
  /// {@macro packaging_exception}
  const PackagingException(this.message);

  /// The message describing the exception.
  final String message;

  @override
  String toString() => 'PackagingException: $message';
}

/// {@template patch_packager}
/// Creates and packages patch artifacts.
/// {@endtemplate }
class PatchPackager {
  /// {@macro patch_packager}
  PatchPackager({
    required File patchExecutable,
  }) : _patchExecutable = patchExecutable {
    if (!patchExecutable.existsSync()) {
      throw FileSystemException(
        'Patch executable does not exist',
        patchExecutable.path,
      );
    }
  }

  final File _patchExecutable;

  /// Create and package a patch of [patchArchive] onto [releaseArchive].
  Future<Directory> packagePatch({
    required File releaseArchive,
    required File patchArchive,
    required ArchiveType archiveType,
    required Directory outputDirectory,
  }) async {
    final directory = switch (archiveType) {
      ArchiveType.aab => await _packageAndroidAabPatch(
          releaseAab: releaseArchive,
          patchAab: patchArchive,
        ),
    };

    return directory.renameSync(outputDirectory.path);
  }

  /// Create and package a patch of [patchAab] onto [releaseAab]. The returned
  /// directory will contain a zip file for each architecture in the release
  /// aab.
  ///
  /// If a libapp.so exists for an architecture in [releaseAab] but not in
  /// [patchAab], a [PackagingException] will be thrown.
  Future<Directory> _packageAndroidAabPatch({
    required File releaseAab,
    required File patchAab,
  }) async {
    // Extract the release and patch aabs to temporary directories.
    //
    // temp_dir
    //  └── release
    //     └── [release aab contents]
    //  └── patch
    //     └── [patch aab contents]
    final extractionDir = Directory.systemTemp.createTempSync();
    final extractedReleaseDir =
        Directory(p.join(extractionDir.path, 'release'));
    await releaseAab.extractZip(outputDirectory: extractedReleaseDir);

    final extractedPatchDir = Directory(p.join(extractionDir.path, 'patch'));
    await patchAab.extractZip(outputDirectory: extractedPatchDir);

    // The base/lib directory in the extracted aab contains a directory for
    // each architecture in the aab. Each of these directories contains a
    // libapp.so file.
    final releaseArchsDir = Directory(
      p.join(extractedReleaseDir.path, 'base', 'lib'),
    );

    final outDir = Directory.systemTemp.createTempSync();
    // For every architecture in the release aab, create a diff and zip it.
    // If a libapp.so exists for an architecture in the release aab but not in
    // the patch aab, throw an exception.
    for (final archDir in releaseArchsDir.listSync().whereType<Directory>()) {
      final archName = p.basename(archDir.path);
      logger.detail('Creating diff for $archName');

      // Get the elf files for the release and patch aabs.
      final relativeArchPath =
          p.relative(archDir.path, from: extractedReleaseDir.path);
      final releaseElf = File(p.join(archDir.path, 'libapp.so'));
      final patchElf = File(
        p.join(extractedPatchDir.path, relativeArchPath, 'libapp.so'),
      );

      // If the release aab is missing a libapp.so, this is likely not a Flutter
      // app. Throw an exception.
      if (!releaseElf.existsSync()) {
        throw PackagingException('Release aab missing libapp.so for $archName');
      }

      // Make sure the patch aab has a libapp.so for this architecture.
      if (!patchElf.existsSync()) {
        throw PackagingException('Patch aab missing libapp.so for $archName');
      }

      // Create a diff file in an output directory named [archName].
      final diffArchDir = Directory(p.join(outDir.path, archName))
        ..createSync(recursive: true);
      final diffFile = File(p.join(diffArchDir.path, 'dlc.vmcode'));
      await _makeDiff(
        base: releaseElf,
        patch: patchElf,
        outFile: diffFile,
      );
      logger.detail('Diff file created at ${diffFile.path}');

      // Write the hash of the pre-diffed patch elf to a file.
      final hash = sha256.convert(await patchElf.readAsBytes()).toString();
      File(p.join(diffArchDir.path, 'hash'))
        ..createSync(recursive: true)
        ..writeAsStringSync(hash);

      // Zip the directory containing the diff file and move it to the output
      // directory.
      final zippedDiff = await diffArchDir.zipToTempFile();
      final zipTargetPath = p.join(outDir.path, '$archName.zip');
      logger.detail('Moving packaged patch to $zipTargetPath');
      zippedDiff.renameSync(zipTargetPath);

      // Clean up.
      diffArchDir.deleteSync(recursive: true);
    }

    return outDir;
  }

  /// Create a binary diff between [base] and [patch]. Returns the path to the
  /// diff file.
  Future<void> _makeDiff({
    required File base,
    required File patch,
    required File outFile,
  }) async {
    logger.detail('Creating diff between ${base.path} and ${patch.path}');
    final args = [
      _patchExecutable.path,
      base.path,
      patch.path,
      outFile.path,
    ];
    final result = await processManager.run(args);

    if (result.exitCode != ExitCode.success.code) {
      throw ProcessException(
        args.first,
        args.sublist(1),
        'Failed to create diff',
        result.exitCode,
      );
    }

    if (!outFile.existsSync()) {
      throw FileSystemException(
        'patch completed successfully but diff file does not exist',
        outFile.path,
      );
    }
  }
}
