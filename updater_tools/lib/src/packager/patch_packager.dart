import 'dart:io';
import 'dart:isolate';

import 'package:archive/archive_io.dart';
import 'package:path/path.dart' as p;
import 'package:updater_tools/src/logger.dart';

/// {@template patch_packager}
/// {@endtemplate }
class PatchPackager {
  /// {@macro patch_packager}
  PatchPackager({
    required this.patchExecutable,
  }) {
    if (!patchExecutable.existsSync()) {
      throw FileSystemException(
        'Patch executable does not exist',
        patchExecutable.path,
      );
    }
  }

  final File patchExecutable;

  Future<Directory> packagePatch({
    required File releaseArchive,
    required File patchArchive,
  }) async {
    if (!releaseArchive.existsSync()) {
      throw FileSystemException(
        'Release archive not exist',
        releaseArchive.path,
      );
    }

    if (!patchArchive.existsSync()) {
      throw FileSystemException(
        'Patch archive not exist',
        patchArchive.path,
      );
    }

    final releaseExtension = p.extension(releaseArchive.path);
    final patchExtension = p.extension(releaseArchive.path);
    if (releaseExtension != patchExtension) {
      throw FileSystemException(
        '''Release and patch archives must have the same extension (found $releaseExtension and $patchExtension)''',
        releaseArchive.path,
      );
    }

    switch (releaseExtension) {
      case '.aab':
        return packageAndroidAabPatch(
          releaseAab: releaseArchive,
          patchAab: patchArchive,
        );
      default:
        throw Exception('Unsupported archive extension: $releaseExtension');
    }
  }

  ///
  Future<Directory> packageAndroidAabPatch({
    required File releaseAab,
    required File patchAab,
  }) async {
    if (!releaseAab.existsSync()) {
      throw FileSystemException('Release aab does not exist', releaseAab.path);
    }

    if (!patchAab.existsSync()) {
      throw FileSystemException('Patch aab does not exist', patchAab.path);
    }

    final extractedReleaseDir = Directory.systemTemp.createTempSync();
    await extractZip(zipFile: releaseAab, outputDirectory: extractedReleaseDir);

    final extractedPatchDir = Directory.systemTemp.createTempSync();
    await extractZip(zipFile: patchAab, outputDirectory: extractedPatchDir);

    final releaseArchsDir = Directory(
      p.join(extractedReleaseDir.path, 'base', 'lib'),
    );

    final outDir = Directory.systemTemp.createTempSync();
    for (final archDir in releaseArchsDir.listSync().whereType<Directory>()) {
      final archName = p.basename(archDir.path);
      // Path to the arch directory within the aab. The patch aab should have
      // the same directory structure.
      final relativeArchPath =
          p.relative(archDir.path, from: extractedReleaseDir.path);
      final releaseElf = File(p.join(archDir.path, 'libapp.so'));
      final patchElf = File(
        p.join(extractedPatchDir.path, relativeArchPath, 'libapp.so'),
      );
      if (!patchElf.existsSync()) {
        logger.err('Patch elf does not exist at ${patchElf.path}');
        continue;
      }

      final diffDir = Directory(p.join(outDir.path, archName))
        ..createSync(recursive: true);

      final diffFile = await _makeDiff(base: releaseElf, patch: patchElf);
      Directory(p.join(diffDir.path, archName)).createSync(recursive: true);
      diffFile.renameSync(p.join(diffDir.path, archName, 'dlc.vmcode'));
      final zippedDiff = await diffDir.zipToTempFile();
      zippedDiff.renameSync(p.join(outDir.path, '$archName.zip'));
      diffDir.deleteSync(recursive: true);
    }

    return outDir;
  }

  /// Create a binary diff between [base] and [patch]. Returns the path to the
  /// diff file.
  Future<File> _makeDiff({
    required File base,
    required File patch,
  }) async {
    logger.detail('Creating diff between ${base.path} and ${patch.path}');
    final outFile =
        File(p.join(Directory.systemTemp.createTempSync().path, 'diff'))
          ..createSync(recursive: true);
    final args = [
      base.path,
      patch.path,
      outFile.path,
    ];
    final result = await Process.run(patchExecutable.path, args);

    if (result.exitCode != 0) {
      throw ProcessException(
        patchExecutable.path,
        args,
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

    return outFile;
  }

  /// Extracts the [zipFile] to the [outputDirectory] directory in a separate
  /// isolate.
  Future<void> extractZip({
    required File zipFile,
    required Directory outputDirectory,
  }) async {
    await Isolate.run(() async {
      final inputStream = InputFileStream(zipFile.path);
      final archive = ZipDecoder().decodeBuffer(inputStream);
      await extractArchiveToDisk(archive, outputDirectory.path);
      inputStream.closeSync();
    });
  }
}

extension DirectoryArchive on Directory {
  /// Copies this directory to a temporary directory and zips it.
  Future<File> zipToTempFile() async {
    final tempDir = await Directory.systemTemp.createTemp();
    final outFile = File(p.join(tempDir.path, '${p.basename(path)}.zip'));
    await Isolate.run(() {
      ZipFileEncoder().zipDirectory(this, filename: outFile.path);
    });
    return outFile;
  }
}
