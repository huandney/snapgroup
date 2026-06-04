# ADR: Acoplamento ao Snapper — a cegueira do boot de resgate justifica sair do wrapper?

- **Status:** Proposto (não decidido) — registra o quadro de decisão.
- **Data:** 2026-06-02
- **Relacionado:** `docs/proposals/standalone-btrfs.md`,
  `docs/proposals/transactional-boot-sync-doctor.md` (Parte 2),
  `docs/incidents/2026-06-02-fat32-interrupted-boot-sync/postmortem.md`

## Contexto

Implementando a Opção C do doctor (restaurar só o membro `root` para casar com
o kernel do `/boot`), descobrimos uma limitação: rodando de dentro de um **boot
de resgate** (snapshot read-only bootado pelo Limine), o `snapper -c root list`
**não enxerga nenhum snapshot de `/`**.

Causa: o snapper lê `/.snapshots`, que é um **subvolume aninhado** e **não entra**
no snapshot — vira um diretório vazio. Bootando `@/.snapshots/660/snapshot` como
`/`, o `/.snapshots` está vazio. Os snapshots reais existem em
`<toplevel>/@/.snapshots/N/snapshot`, mas o snapper, a partir dali, é cego a eles.
`home`/`root_home` funcionam porque `/home` e `/root` são os subvolumes **vivos**
(`@home`, `@root`), não snapshots.

A pergunta levantada: isso é boa justificativa para o snapgroup deixar de ser um
wrapper do Snapper e ler/gerir BTRFS direto (ambição já registrada em
`standalone-btrfs.md`)?

## Reframe: o snapgroup já é híbrido

O snapgroup **não é** wrapper puro:

- **Rollback** já é btrfs cru (rename-swap de subvolumes), não `snapper rollback`.
- Do snapper ele usa só: **listar snapshots**, o **mapa config→subvolume**, os
  **metadados** (data, descrição, e o `snapgroup-id` no `userdata`), e
  **criação/cleanup**.

A cegueira do resgate é **só na ponta de leitura**. Então o que está em jogo é
trocar a leitura via CLI por leitura direta do toplevel — não abandonar o snapper.

## A justificativa é real, mas fraca

Dois fatos a enfraquecem:

1. **A Opção A já recupera o sistema do resgate** sem enumerar snapshots de `/`:
   ela lê o `/@` do toplevel montado e sincroniza o `/boot`. O caso de
   recuperação já está coberto; a Opção C (mudar o `/` de dentro do resgate) é
   conveniência, não necessidade.
2. **Compatibilidade com o ecossistema.** O `limine-snapper-sync` e o snapper
   convivem nesta máquina. Se o snapgroup criar/gerir snapshots fora do snapper,
   esses snapshots somem da visão do snapper e do limine → dessincronia.

## Custo, por escopo

| Escopo | O que muda | Custo | Risco |
| --- | --- | --- | --- |
| **Cirúrgico** — ler snapshots do toplevel; manter snapper p/ criação/cleanup/config-map | varredura de `@/.snapshots/*` + parse do `info.xml` no lugar de `snapper::list` | Moderado | troca acoplamento-CLI por acoplamento-ao-formato (`info.xml`); cobrir layout nested/flat |
| **Médio** — leitura toplevel para todos os configs | idem + reorganizar `group::list_groups` | Médio | mais superfície |
| **Total** — ciclo de vida próprio (criar/cleanup/metadados) | reimplementar criação e cleanup; parar de usar snapper | Alto | snapper/limine deixam de ver os snapshots do snapgroup |

**Espinho do escopo cirúrgico:** para agrupar, precisa-se do `snapgroup-id`, que
vive no `userdata` → dentro do `info.xml` (XML). Parsear exige **(a)** uma crate
de XML — que **bate com a política de deps** (tudo via pacman, nada de crate nova
sem aceite) — ou **(b)** um parser hand-rolled do `info.xml` (formato estável,
mas código frágil a manter). `btrfs subvolume list` sozinho não resolve: dá
paths/IDs, não data/descrição/userdata.

## Alternativa que reduz acoplamento sem reescrever a leitura

O snapgroup **parar de guardar o `snapgroup-id` no `userdata` do snapper** e
manter o **próprio índice de grupos** (um arquivo que ele controla). Aí a leitura
por toplevel não dependeria de parsear `info.xml` para o id — só para
data/descrição (deriváveis do subvol). Meio-termo útil se um dia for por esse
caminho.

## Recomendação

Não vale "deixar de ser wrapper" por causa da cegueira do resgate:

- A **Opção A** já cobre a recuperação de dentro do resgate.
- O custo total derruba a compatibilidade com o snapper/limine.

Se e quando "mudar o `/` de dentro do resgate" virar requisito real, fazer o
**escopo cirúrgico** (leitor de snapshots pelo toplevel), mantendo o snapper para
criação, cleanup e mapa de configs. Prioridade **baixa**, atrás de fechar o que
está em andamento. A decisão maior (standalone) segue em `standalone-btrfs.md`;
esta ADR só registra que a cegueira do resgate **não** é gatilho suficiente para
ela.
