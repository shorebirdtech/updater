import 'dart:io';

import 'package:mason_logger/mason_logger.dart';
import 'package:mocktail/mocktail.dart';
import 'package:path/path.dart' as p;
import 'package:process/process.dart';
import 'package:scoped_deps/scoped_deps.dart';
import 'package:test/test.dart';
import 'package:updater_tools/src/artifact_type.dart';
import 'package:updater_tools/src/extensions/archive.dart';
import 'package:updater_tools/src/logger.dart';
import 'package:updater_tools/src/packager/patch_packager.dart';
import 'package:updater_tools/src/process.dart';

import '../../matchers/has_prefix.dart';

class _MockLogger extends Mock implements Logger {}

class _MockProcessManager extends Mock implements ProcessManager {}

void main() {
  group(PatchPackager, () {
    late Logger logger;
    late ProcessManager processManager;
    late PatchPackager packager;

    R runWithOverrides<R>(R Function() body) {
      return runScoped(
        body,
        values: {
          loggerRef.overrideWith(() => logger),
          processManagerRef.overrideWith(() => processManager),
        },
      );
    }

    Future<File> createAab({
      required String name,
      required List<String> archs,
    }) async {
      final tempDir = Directory.systemTemp.createTempSync();
      final libDir = Directory(p.join(tempDir.path, 'base', 'lib'))
        ..createSync(recursive: true);
      for (final arch in archs) {
        File(p.join(libDir.path, arch, 'libapp.so'))
            .createSync(recursive: true);
      }
      final zippedAab = await tempDir.zipToTempFile();
      return zippedAab.renameSync('$name.aab');
    }

    setUp(() {
      final tempDir = Directory.systemTemp.createTempSync();
      final patchExecutable = File(p.join(tempDir.path, 'patch'))
        ..createSync(recursive: true);

      logger = _MockLogger();
      processManager = _MockProcessManager();

      // `patch` is invoked like:
      //   ```
      //   $ patch baseSnapshot patchSnapshot outputFile
      //   ````
      when(
        () => processManager.run(any(that: hasPrefix([patchExecutable.path]))),
      ).thenAnswer(
        (invocation) async {
          final commandParts =
              invocation.positionalArguments.first as List<Object>;
          final outputFilePath = commandParts.last as String;
          File(outputFilePath)
            ..createSync()
            ..writeAsStringSync('contents');
          return ProcessResult(0, ExitCode.success.code, '', '');
        },
      );

      packager = PatchPackager(patchExecutable: patchExecutable);
    });

    group('packagePatch with .aabs', () {
      group('when patch aab is missing archs present in release aab', () {
        test('throws an exception', () async {
          final outDir = Directory(
            p.join(
              Directory.systemTemp.createTempSync().path,
              'out',
            ),
          );
          final releaseAab = await createAab(
            name: 'release',
            archs: ['arch1', 'arch2'],
          );
          final patchAab = await createAab(name: 'patch', archs: ['arch1']);

          await expectLater(
            () => runWithOverrides(
              () => packager.packagePatch(
                releaseArchive: releaseAab,
                patchArchive: patchAab,
                archiveType: ArchiveType.aab,
                outputDirectory: outDir,
              ),
            ),
            throwsA(
              isA<PackagingException>().having(
                (e) => e.message,
                'message',
                'Patch aab missing libapp.so for arch2',
              ),
            ),
          );
        });
      });

      test('outputs one zipped patch file per arch found in the release aab',
          () async {
        final outDir = Directory(
          p.join(
            Directory.systemTemp.createTempSync().path,
            'out',
          ),
        );

        expect(outDir.existsSync(), isFalse);

        const archs = ['arm64-v8a', 'armeabi-v7a', 'x86_64'];
        final releaseAab = await createAab(name: 'release', archs: archs);
        final patchAab = await createAab(name: 'patch', archs: archs);

        await runWithOverrides(
          () => packager.packagePatch(
            releaseArchive: releaseAab,
            patchArchive: patchAab,
            archiveType: ArchiveType.aab,
            outputDirectory: outDir,
          ),
        );

        // The outputDirectory should have been created.
        expect(outDir.existsSync(), isTrue);

        // The outputDirectory should contain a zip file for each arch.
        final outDirContents = outDir.listSync().whereType<File>();
        expect(outDirContents, hasLength(3));
        expect(
          outDirContents.map((f) => p.basename(f.path)),
          containsAll(archs.map((a) => '$a.zip')),
        );

        // Each zip file should decompress to a directory with the given arch
        // name containing the dlc.vmcode file produced by the patch executable.
        for (final zipFile in outDirContents) {
          final tempDir = Directory.systemTemp.createTempSync();
          await zipFile.extractZip(outputDirectory: tempDir);
          final extractedContents = tempDir.listSync();

          // Contents of the zip file should be a single directory with the
          // same name as the zip file (the arch of the patch file it contains).
          expect(extractedContents, hasLength(1));
          final patchFile = extractedContents.single;
          expect(patchFile, isA<File>());
          expect(p.basename(patchFile.path), equals('dlc.vmcode'));
          expect((patchFile as File).readAsStringSync(), equals('contents'));
        }
      });
    });
  });
}
