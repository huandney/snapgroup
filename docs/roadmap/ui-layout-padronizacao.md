# UI / layout — limitação de resize e padronização de separadores

Backlog de polimento da camada de UI (`src/ui/`). Dois temas: uma limitação
estrutural conhecida (resize ao vivo) e a padronização de como os campos são
separados nas telas.

## Limitação conhecida: resize ao vivo nos pickers

Redimensionar o terminal no meio de um picker corta/embaralha o texto estático
impresso acima do widget (ex.: o bloco `Checkpoint / Data / Descrição` na
seleção de membros).

**Causa.** O `dialoguer` bloqueia lendo tecla e redesenha apenas as próprias
linhas (prompt + itens). O que é impresso acima dele é estático e não é
repintado. No resize, o emulador reflui/corta o buffer da tela alternativa, e as
linhas quebradas para a largura antiga ficam cortadas na nova. O `dialoguer` não
expõe função de render sob demanda nem um jeito de interromper o `read`
bloqueante, então não há hook de "redesenhe no `SIGWINCH`".

**Por que não há add-on.** Detectar o resize é trivial (`signal-hook`,
`crossterm` com `Event::Resize`), mas inútil: o gargalo é o loop fechado do
`dialoguer`, não a falta de detecção. Trocar por outra lib de prompt
(`inquire`, `requestty`, `cliclack`) não muda nada — todas compartilham o mesmo
modelo bloqueante.

**Opções de fix real.**
1. **Mitigar** (escolhido): minimizar o conteúdo estático acima do widget.
   Descrições reais são curtas; colapsar a descrição multi-linha para uma linha
   torna o caso comum 100% robusto. Mudança cirúrgica.
2. **Picker próprio sobre `crossterm`** (~150 linhas): um event loop procedural
   (`event::read()` entrega tecla e resize no mesmo stream) que redesenha tudo a
   cada evento. Sem framework, alinhado ao estilo do projeto. Vale só se
   resize-robustez virar requisito do wizard inteiro.
3. **`ratatui`** (immediate-mode): resolve resize e destrava preview pane,
   descrição rolável, progresso ao vivo, busca/filtro. Mas reescreve toda a
   superfície interativa e troca o caminho-reto procedural por uma máquina de
   estados — decisão de produto, não de bugfix.

**Decisão atual:** mitigar (opção 1). Reavaliar 2/3 se o projeto decidir virar um
app TUI navegável em vez de um CLI com wizards lineares.

## Padronização de separadores / layout

Hoje convivem dois idiomas sem regra clara: o `·` (dot) aparece em umas telas e
colunas alinhadas em outras. Regra proposta, atrelada à **forma do dado**:

1. **Tabela (N linhas, mesmos campos)** → colunas alinhadas, sem dots. O olho
   varre na vertical; dots numa lista multi-linha atrapalham porque os campos não
   se alinham. Ex.: `snapg list`, apagar checkpoints, pickers de membros.
2. **Linha-resumo única (1 linha de contexto)** → campos separados por ` · `.
   Não há o que alinhar na vertical; o dot delimita campos de forma limpa. Ex.:
   `Alvo · sistema atual`, resumo do Regret, `etapa X de Y · msg`, cabeçalhos de
   revisão.
3. **Bloco rótulo→valor (vertical)** → conector de árvore `├─/└─` + coluna de
   rótulo alinhada. Ex.: diagnóstico do doctor, árvore de membros na revisão.

**Espaçamento:** padronizar o separador-de-campo como `  ·  ` (dois espaços) nos
resumos; reservar o ` · ` (um espaço) para separar cláusulas dentro de uma frase
de item (ex.: `ajusta o /boot · mantém root e home`), onde o ` — ` (em-dash) já
faz o corte primário.

**Pendente:** auditar cada tela e alinhar onde diverge da regra. A lista de
apagar já foi convertida de dots para colunas (caso 1).
