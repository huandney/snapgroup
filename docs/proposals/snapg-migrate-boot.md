# Proposta: `snapg migrate boot`

- **Status:** proposta futura, não implementada.
- **Data:** 2026-06-09.
- **Relacionado:** `docs/architecture/boot-and-standalone-decision.md`,
  `docs/architecture/initramfs-stale-on-restore.md`,
  `docs/proposals/transactional-boot-sync-doctor.md`.

## 0. TL;DR

`snapg migrate boot` será um módulo separado para transicionar a arquitetura de
boot entre:

- `/boot` em FAT32/ESP, fora dos snapshots Btrfs;
- `/boot` dentro do Btrfs, versionado junto com o root.

O comando deve detectar o layout atual, mostrar um plano, pedir confirmação e só
então aplicar. O objetivo principal é permitir migrar para `/boot` em Btrfs,
eliminando a classe de bugs em que root e `/boot` ficam fora de sincronia após
restore.

Regra central: **snapshots antigos criados antes da migração não viram
automaticamente snapshots nativos do novo layout**. Eles precisam de um caminho de
compatibilidade no restore.

---

## 1. Por que isto existe

Com `/boot` em FAT32, o rollback do root é atômico, mas kernel/initramfs ficam fora
do snapshot. O snapg precisa sincronizar `/boot` depois do restore, copiando
`vmlinuz`, regenerando `initramfs` e atualizando hashes do bootloader quando
necessário. Isso funciona, mas mantém uma etapa cross-filesystem no caminho crítico.

Com `/boot` dentro do Btrfs, o snapshot de root inclui:

- `/boot/vmlinuz*`;
- `/boot/initramfs*`;
- configs de boot que morem no rootfs;
- `/usr/lib/modules`;
- configs de `mkinitcpio`.

Nesse modelo, restaurar o root também restaura o boot correspondente. A operação
fica atomicamente consistente no Btrfs e o sync FAT32 deixa de ser necessário para
restore comum.

---

## 2. UX desejada

Uso humano principal:

```bash
snapg migrate
```

O comando detecta o layout e apresenta uma proposta.

Exemplo quando `/boot` está em FAT32:

```text
Boot atual
├─ /boot: vfat em /dev/nvme0n1p1
├─ root: btrfs subvol /@
└─ risco: kernel/initramfs ficam fora dos snapshots

Transição recomendada
└─ mover /boot para dentro do Btrfs
   ├─ ESP passará a montar em /efi
   ├─ kernel/initramfs ficarão em @/boot
   ├─ fstab será ajustado
   └─ entradas do bootloader serão atualizadas

Continuar? [s/N]
```

Exemplo quando `/boot` já está em Btrfs:

```text
Boot atual
├─ /boot: btrfs dentro do root
└─ snapshots incluem kernel/initramfs

Transição disponível
└─ mover /boot para ESP/FAT32 separado

Continuar? [s/N]
```

Modos explícitos para automação e debug:

```bash
snapg migrate boot status
snapg migrate boot --dry-run
snapg migrate boot --to btrfs
snapg migrate boot --to fat32
```

`--dry-run` deve ser o modo natural de inspeção. Nenhuma escrita deve acontecer
antes de confirmação explícita.

---

## 3. Snapshot antigo após migração

Este é o ponto mais importante do design.

Antes da migração:

```text
@/.snapshots/N/snapshot/
└─ boot/              # geralmente vazio ou só mountpoint

/boot                 # FAT32 fora do snapshot, com kernel/initramfs reais
```

Depois da migração:

```text
@/
└─ boot/
   ├─ vmlinuz...
   └─ initramfs...
```

Se o usuário restaurar um snapshot antigo, criado antes da migração, o root
restaurado pode não conter o `/boot` interno necessário. Portanto:

```text
snapshot pós-migração
  → restore atômico; /boot vem junto

snapshot pré-migração
  → precisa caminho de compatibilidade para reconstruir /boot no novo layout
```

O snapg deve registrar a fronteira da migração e tratar snapshots anteriores como
`pre-boot-migration`.

Metadado sugerido:

```text
/var/lib/snapgroup/migration.json
```

Formato inicial:

```json
{
  "version": 1,
  "boot_layout": "btrfs",
  "migrated_at": "2026-06-09T00:00:00Z",
  "first_native_checkpoint": 1790000000
}
```

`first_native_checkpoint` deve apontar para o primeiro checkpoint criado depois da
migração, quando `/boot` já está dentro do Btrfs.

---

## 4. Regra de restore após migração

Quando o layout atual for `/boot` em Btrfs:

1. Se o checkpoint é pós-migração:
   - restore normal;
   - não rodar `sync_fat32`;
   - não regenerar initramfs;
   - boot vem do snapshot.

2. Se o checkpoint é pré-migração:
   - detectar que o snapshot não tem `/boot` nativo válido;
   - aplicar caminho de compatibilidade;
   - opções possíveis:
     - restaurar o root e regenerar `/boot` dentro do Btrfs a partir do root
       restaurado;
     - ou bloquear o restore direto e orientar um comando de recovery;
     - ou manter fallback FAT32 temporário durante uma fase de transição.

A opção preferida para compatibilidade é regenerar `/boot` dentro do Btrfs após o
restore pré-migração. Isso preserva a capacidade de voltar a snapshots antigos,
sem exigir que eles tenham artefatos que não existiam quando foram criados.

---

## 5. Migração FAT32 → Btrfs

Responsabilidades do `snapg migrate boot --to btrfs`:

1. Detectar:
   - `/` é Btrfs;
   - root usa subvolume conhecido;
   - `/boot` atual é FAT32/ESP separado;
   - bootloader suportado consegue ler Btrfs no ambiente local.

2. Planejar:
   - novo mountpoint da ESP, por exemplo `/efi`;
   - novo `/boot` dentro do root Btrfs;
   - alterações em `/etc/fstab`;
   - alterações de bootloader;
   - backup de arquivos alterados.

3. Aplicar:
   - criar diretórios necessários;
   - copiar conteúdo de `/boot` para o novo `@/boot`;
   - mover/remontar ESP para `/efi`;
   - atualizar `/etc/fstab`;
   - atualizar configuração do bootloader;
   - criar metadado de migração.

4. Verificar:
   - `/boot` atual é Btrfs;
   - ESP está acessível em `/efi`;
   - kernel/initramfs existem em `/boot`;
   - entradas do bootloader apontam para artefatos válidos;
   - hashes, quando usados pelo bootloader, batem.

5. Criar ou recomendar checkpoint pós-migração:
   - o primeiro checkpoint pós-migração vira a fronteira nativa.

---

## 6. Migração Btrfs → FAT32

A migração reversa deve existir, mas precisa avisar que reintroduz o problema de
boot fora do snapshot.

Responsabilidades do `snapg migrate boot --to fat32`:

1. Detectar ESP/FAT32 disponível.
2. Copiar kernel/initramfs atuais para FAT32.
3. Ajustar `/etc/fstab` para montar FAT32 em `/boot`.
4. Atualizar configuração do bootloader.
5. Marcar o layout atual como `fat32`.
6. Reativar o caminho de sync FAT32 no restore.

Aviso obrigatório:

```text
Esta migração coloca kernel/initramfs fora dos snapshots Btrfs.
Restores de root voltarão a exigir sincronização de /boot.
```

---

## 7. Segurança e rollback da migração

`snapg migrate` não deve ser uma sequência opaca de comandos. Ele precisa produzir
um plano e manter backups dos arquivos alterados.

Arquivos sensíveis:

- `/etc/fstab`;
- configuração do bootloader;
- layout de mountpoints `/boot` e `/efi`;
- arquivos de boot existentes.

Regras:

- `--dry-run` não escreve nada.
- `--apply` exige confirmação.
- falha antes do ponto de commit deve tentar rollback automático;
- falha depois do ponto de commit deve imprimir recuperação manual precisa;
- nunca remover a ESP nem apagar artefatos antigos na primeira versão.

O primeiro release da feature deve preferir deixar sobras recuperáveis a tentar
limpeza agressiva.

---

## 8. Integração com bootloaders

O módulo de migração deve separar core genérico e adaptadores.

Core:

- detectar filesystems e mountpoints;
- mover/copy boot tree;
- editar fstab;
- registrar metadata de migração.

Adaptadores:

- Limine;
- GRUB;
- systemd-boot, se aplicável no futuro.

Para Limine, pendências técnicas:

- confirmar sintaxe exata para referenciar kernel/initramfs no Btrfs;
- confirmar se `limine.conf` fica na ESP ou no Btrfs;
- confirmar comportamento dos hashes BLAKE2B quando os artefatos estão em Btrfs;
- garantir que entradas de snapshot apontem para o mesmo subvolume que o root.

O suporte inicial pode ser Limine-only se isso for explicitamente comunicado, mas
o desenho do módulo não deve acoplar o core a `limine-snapper-sync`.

---

## 9. Fases de implementação

### Fase 1 — Diagnóstico

Implementar:

```bash
snapg migrate boot status
```

Sem escrita. Deve mostrar:

- filesystem de `/boot`;
- se `/boot` está dentro do root Btrfs;
- mountpoint da ESP;
- root subvol atual;
- recomendação de migração.

### Fase 2 — Planejamento

Implementar:

```bash
snapg migrate boot --dry-run
```

Gera plano detalhado, incluindo arquivos que seriam alterados.

### Fase 3 — Migração FAT32 → Btrfs

Implementar `--to btrfs --apply` para o layout do usuário atual, com Limine.

### Fase 4 — Compatibilidade de snapshots pré-migração

Implementar regra de restore para checkpoints antigos.

### Fase 5 — Migração reversa

Implementar `--to fat32 --apply`, com avisos fortes.

---

## 10. Decisões ainda abertas

- O primeiro release deve suportar só Limine ou já preparar GRUB?
- `limine.conf` deve ficar na ESP ou no Btrfs?
- Como identificar de forma robusta `first_native_checkpoint`?
- O restore pré-migração deve regenerar `/boot` automaticamente ou pedir confirmação?
- Secure Boot é requisito?
- Qual é o formato final de `migration.json`?

---

## 11. Recomendação atual

Não implementar `snapg migrate` como parte do fix de initramfs/DKMS. Primeiro manter
o caminho FAT32 correto e seguro. Depois implementar `snapg migrate boot` como
feature separada, começando por `status` e `--dry-run`.

O objetivo de longo prazo é claro: `/boot` em Btrfs elimina a classe inteira de
sync de boot no restore. O risco está na migração, não no estado final.
