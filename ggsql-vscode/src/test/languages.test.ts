import * as assert from 'assert';
import * as vscode from 'vscode';
import { isGgsqlDocument, sqlFilesEnabled } from '../languages';

function setSqlFiles(value: boolean | undefined): Thenable<void> {
	return vscode.workspace
		.getConfiguration('ggsql')
		.update('enableSqlFiles', value, vscode.ConfigurationTarget.Global);
}

function doc(language: string): Thenable<vscode.TextDocument> {
	return vscode.workspace.openTextDocument({ content: 'SELECT 1\n', language });
}

suite('isGgsqlDocument', () => {
	teardown(async () => {
		await setSqlFiles(undefined);
	});

	test('defaults to attaching to sql files', async () => {
		// Asserts the shipped default, so it must clear any value a previous
		// test or file may have left behind rather than rely on teardown alone.
		await setSqlFiles(undefined);
		assert.strictEqual(sqlFilesEnabled(), true);
	});

	test('a ggsql document always qualifies', async () => {
		const document = await doc('ggsql');
		await setSqlFiles(false);
		assert.strictEqual(isGgsqlDocument(document), true);
	});

	test('a sql document qualifies only while enableSqlFiles is on', async () => {
		const document = await doc('sql');

		await setSqlFiles(true);
		assert.strictEqual(isGgsqlDocument(document), true);

		await setSqlFiles(false);
		assert.strictEqual(isGgsqlDocument(document), false);
	});

	test('an unrelated language never qualifies', async () => {
		const document = await doc('plaintext');
		assert.strictEqual(isGgsqlDocument(document), false);
	});

	test('undefined does not qualify', () => {
		assert.strictEqual(isGgsqlDocument(undefined), false);
	});
});
