# Notas da sessão (atualizado 2026-04-26 noite)

## Estado atual

- Sistema **limpo** após bagunça de testes do undo. Toplevel só com subvols válidos: `@ @home @cache @log @root @srv @tmp`.
- `/.snapshots` e `/home/.snapshots` recriados como subvols btrfs (vazios — histórico antigo perdido na limpeza, baseline novo).
- Snapper configs intactas: `root → /` e `home → /home`. Ambas funcionais.
- Código compila limpo (`cargo check` + `cargo clippy --release` sem warnings).

## Fix aplicado — nested .snapshots

**Problema:** No CachyOS (e openSUSE), `/.snapshots` e `/home/.snapshots` são subvols **aninhados** dentro de `@` e `@home`. O rename-swap original mandava `.snapshots` junto com o subvol arquivado — novo subvol ativo ficava só com placeholder vazio, snapper perdia o histórico.

**Solução em `src/rollback.rs`:**
- Após o rename-swap, se `backup/.snapshots` é subvol, faz `fs::rename(backup/.snapshots → new_current/.snapshots)`. Rename de subvol entre subvols irmãos no mesmo fs é metadata-only, atômico.
- `revert_done` faz o simétrico: move `.snapshots` de volta pro backup antes do swap reverso, evitando que `.snapshots` caia no `discard` e seja deletado.
- Helper novo em `src/btrfs.rs`: `is_subvolume()` (check via exit code de `btrfs subvolume show`).

## Naming alinhado com btrfs-assistant

Trocado `@_backup_<epoch>` por `@_backup_<YYYY-MM-DD_HH:MM:SS>` (timestamp local, granularidade segundo). Helper `now_local_label()` em `btrfs.rs` shell-out pra `date +%Y-%m-%d_%H:%M:%S` — evita dep de chrono.

Exemplo do nome agora: `@home_backup_2026-04-26_19:57:24`.

## Limitação conhecida — multi-disk

Em `commands.rs::undo`, monta toplevel só do filesystem de `/`:

```rust
let uuid = btrfs::fs_uuid("/")?;
btrfs::mount_toplevel(&uuid, &mount_path)?;
```

**Premissa:** todas as configs snapper vivem no mesmo filesystem btrfs (mesmo UUID). Caso de quebra: SSD btrfs pra `/` + HDD btrfs separado pra `/home`, ambos com snapper config. Subvol `@home` não estaria no mount toplevel do `/` → undo falha.

Cenários que NÃO quebram (auto-resolvem):
- `/home` em ext4/xfs (snapper não tem config → snap-tools nem tenta)
- Disco único particionado (mesmo btrfs)
- Pool btrfs multi-device (vira um filesystem só pro kernel)

**Fix futuro:** agrupar membros por UUID em `undo`, montar/desmontar toplevel por UUID. ~30 linhas mecânicas em `commands.rs`. Adia até precisar — documenta no README como limitação.

## Status `/root`

Snap-tools auto-descobre via `snapper list-configs`. Pra incluir `/root` (que no CachyOS é subvol `@root` separado):

```bash
sudo snapper -c root_home create-config /root
# opcional: reduzir retenção timeline (root muda pouco)
sudo snapper -c root_home set-config TIMELINE_CREATE=no
```

Próximo `snap-save` agrupa os 3 (root + home + root_home). Sem mudança no código.

Em distros que botam `/root` dentro do `@` (sem subvol próprio), `create-config /root` falha — teria que converter primeiro. Não é nosso caso.

## Arquivos do projeto

```
src/
├── main.rs       — dispatch
├── cli.rs        — clap derive (Save/Undo/List/Delete)
├── sudo.rs       — re-exec via sudo se UID != 0
├── snapper.rs    — list_configs, list, create, delete, config_subvolume
├── btrfs.rs      — mount_toplevel, create_snapshot, fs_uuid, is_subvolume, now_local_label
├── group.rs      — GroupId = epoch, agrupa por userdata snap-tools-id
├── rollback.rs   — rename-swap + nested .snapshots handling + revert_done
└── commands.rs   — save/undo/list/delete
```

PKGBUILD: `depends=('snapper' 'btrfs-progs' 'util-linux' 'fzf')`. Build via `makepkg -si` na raiz.

## Próximos passos

1. **Testar fluxo completo** com fix do nested aplicado:
   ```bash
   cargo run -- save "teste fix nested"
   cargo run -- list
   cargo run -- undo
   sudo snapper -c root list   # tem que mostrar histórico
   sudo snapper -c home list   # idem
   findmnt -no SOURCE / /home  # @ e @home (não backup)
   ```
2. Validar que reboot não quebra (`/.snapshots` continua subvol acessível pós-boot)
3. (Opcional, stage 2) Detecção de gaps: warn quando subvol top-level sem snapper config
4. Documentar no README: limitação multi-disk, como adicionar `/root`, pré-requisitos
5. Stage 2/3: publicar GitHub + AUR

## UUID do filesystem (referência)

`28b7475c-8589-4710-a16c-cfe60b0b1218`


-------------
# Próximos ajustes:
- Ao dar undo mais de uma vez, apagar o snapg_undo anterior, sempre mantendo um para limpeza
- Quando rodarmos snapg delete, apagar o o snapg_undo associado a ele
- Ao dar comando list mostrar tambem o snapg_undo mas temos que pensar em uma forma para ter fácil vizualização entre os instantâneos normais e os provenientes do undo
- Major: Tornar o código compativel com recuperação em conjunto com mais de um mesmo ssd, ou seja com diferentes uuid
