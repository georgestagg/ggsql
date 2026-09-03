import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';

const EXTENSION_ID = 'ggsql.ggsql';

// Directories created by openFileNamed, removed in suiteTeardown below.
const tempDirs: string[] = [];

/**
 * Opens a real file on disk so the workbench resolves its language from the
 * contributed extensions. `openTextDocument({content})` would let the test pick
 * the language itself, which is the thing under test here.
 */
async function openFileNamed(name: string, content: string): Promise<vscode.TextDocument> {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ggsql-test-'));
	tempDirs.push(dir);
	const file = path.join(dir, name);
	fs.writeFileSync(file, content);
	return vscode.workspace.openTextDocument(file);
}

suite('activation', () => {
	suiteSetup(async () => {
		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(extension, `extension ${EXTENSION_ID} not found`);
		await extension.activate();
	});

	test('the extension activates', () => {
		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.strictEqual(extension?.isActive, true);
	});

	test('ggsql file extensions resolve to the ggsql language', async () => {
		for (const name of ['q.gsql', 'q.ggsql', 'q.ggsql.sql']) {
			const document = await openFileNamed(name, 'SELECT 1\n');
			assert.strictEqual(document.languageId, 'ggsql', `${name} resolved wrongly`);
		}
	});

	test('createNewFile opens an untitled ggsql document', async () => {
		await vscode.commands.executeCommand('ggsql.createNewFile');
		const editor = vscode.window.activeTextEditor;
		assert.ok(editor, 'no active editor after createNewFile');
		assert.strictEqual(editor.document.languageId, 'ggsql');
		assert.strictEqual(editor.document.isUntitled, true);
	});

	test('run commands are unavailable outside Positron', async () => {
		// Every command that executes code is registered after activate() returns
		// early when the Positron API is absent, so invoking one here must reject.
		await assert.rejects(
			() => Promise.resolve(vscode.commands.executeCommand('ggsql.runQuery')),
			/not found/i,
		);
	});

	teardown(async () => {
		await vscode.commands.executeCommand('workbench.action.closeAllEditors');
	});

	suiteTeardown(() => {
		for (const dir of tempDirs) {
			fs.rmSync(dir, { recursive: true, force: true });
		}
	});
});
