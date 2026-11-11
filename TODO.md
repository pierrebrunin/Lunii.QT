# Idées reportées

Pistes repérées (audit du code de novembre 2026, ou en cours de route) et
laissées pour plus tard. Retirer une entrée une fois traitée.

## Interface

- **En-têtes de section vides** dans `lunii-gui/src/app/mod.rs` (« Devices »,
  « Story list ») : le code correspondant est dans `devices.rs` et
  `stories.rs`.
- **Découper `App::connect()`** (`app/mod.rs`, ~160 lignes) par écran : liste,
  podcasts, dialogues. Même idée pour les branches longues de
  `App::action()` (`app/actions.rs`, ~200 lignes).
- **Un seul mécanisme pour ignorer les résultats périmés** : `devices.scan`,
  `durations.generation` et les jetons de la recherche de podcasts font la
  même chose de trois façons.

## Fichiers de configuration

- **Écritures non atomiques, fichiers illisibles remis à zéro** :
  `podcasts.json` (`podcast/state.rs`), `settings.json` (`settings.rs`) et
  `third-party.db` (`db.rs`) sont écrits par `std::fs::write`. Un arrêt en
  pleine écriture laisse un fichier tronqué. À la lecture suivante, il est
  traité comme vide (`unwrap_or_default`) puis écrasé, ou supprimé
  (`third-party.db`) : podcasts suivis, épisodes déjà importés ou titres des
  histoires tierces perdus sans message. Piste : une écriture commune
  (fichier temporaire puis `rename`) et, si la lecture échoue, renommer le
  fichier en `.corrompu` avec un avertissement au lieu de l'écraser.
- **`podcasts.json` sans verrou** : le worker (`record_imported` pendant un
  import de podcast) et l'interface (`follow`, `unfollow`) le lisent, le
  modifient et le réécrivent chacun de leur côté. Une modification peut
  écraser l'autre. `db.rs` a déjà un `FILE_LOCK` pour ce cas.

## Erreurs ignorées

- `app/podcast.rs:904` ignore l'échec de `podcast::follow` (la ligne 562 le
  journalise).
- Les `open::that` (dossier de l'appareil, des journaux, page des versions)
  échouent sans message si aucun programme ne peut ouvrir la cible.

## Appareils

- **Index réécrit en deux temps** : `update_pack_index` (Lunii `.pi`, Flam
  `etc/library/list`) supprime l'index puis l'écrit. Si l'appareil est
  débranché entre les deux, l'index est vide. Les histoires restent
  récupérables (Appareil / Histoires perdues), mais écrire un fichier
  temporaire puis le renommer éviterait ce cas.
- **Rembourrage sur Flam** : une histoire Lunii mise sur une Flam a ses
  `ri` / `si` / `li` complétés à un multiple de 16 octets (AES) ; l'export en
  clair depuis la Flam garde ce rembourrage (test
  `v2_archive_imports_on_lunii_and_flam`). À vérifier sur une vraie Lunii :
  si elle refuse ces fichiers, tronquer à la taille d'origine à l'export.
- **Sauvegarde chiffrée des histoires officielles sur Lunii v1/v2** : seul
  modèle où elles ne peuvent pas être sauvegardées du tout (la Flam et la v3
  font une sauvegarde chiffrée, restaurable sur le même appareil).
  `common::export_backup_story` existe déjà ; reste à vérifier l'import de ces
  sauvegardes, avec un test d'aller-retour.

## Tests

- **CLI** : aucun test. Quelques parcours de bout en bout sur une fausse Lunii
  (`list`, `import` d'une archive et d'un album, `export`, `remove`).
