/*
 * Which documents ggsql attaches to.
 *
 * ggsql is a superset of SQL, so the kernel can execute a plain `.sql` file.
 * The extension therefore offers its run affordances on documents tokenized as
 * `sql` as well as `ggsql`, without claiming the `.sql` file association. That
 * keeps the richer built-in SQL grammar in place for plain SQL, and leaves
 * language-scoped features from other SQL extensions working.
 *
 * Users who want the full ggsql treatment for `.sql` files (ggsql syntax
 * highlighting, the ggsql file icon) set `files.associations` instead, which
 * outranks every extension-contributed association. `sqlAssociation.ts` offers
 * to do that for them.
 */

import * as vscode from 'vscode';

export const GGSQL_LANGUAGE_ID = 'ggsql';
export const SQL_LANGUAGE_ID = 'sql';

/**
 * Language ids the CodeLens provider registers against. Both are registered
 * unconditionally; `isGgsqlDocument` gates `sql` at request time so that
 * toggling the setting takes effect without re-registering providers.
 */
export const CELL_LANGUAGE_IDS = [GGSQL_LANGUAGE_ID, SQL_LANGUAGE_ID];

/**
 * Whether ggsql offers its run affordances in plain `.sql` files.
 *
 * The effective default in practice is the `ggsql.enableSqlFiles` default declared in
 * `package.json`'s configuration schema, not the fallback argument below: once a setting
 * is registered there, VS Code resolves `.get()` against that schema default before
 * falling back to the argument passed here. The `true` below is defensive documentation
 * for the (registered) default, not the value that actually applies.
 */
export function sqlFilesEnabled(): boolean {
	return vscode.workspace
		.getConfiguration('ggsql')
		.get<boolean>('enableSqlFiles', true);
}

/** True when ggsql is willing to execute the contents of `document`. */
export function isGgsqlDocument(document: vscode.TextDocument | undefined): boolean {
	if (!document) {
		return false;
	}
	if (document.languageId === GGSQL_LANGUAGE_ID) {
		return true;
	}
	return document.languageId === SQL_LANGUAGE_ID && sqlFilesEnabled();
}
