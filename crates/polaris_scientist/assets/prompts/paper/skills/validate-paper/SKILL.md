<!-- Source: AI-Scientist-v3 src/agents/skills/validate-paper/SKILL.md (Apache-2.0; see LICENSE and NOTICE in the folder assets/prompts/paper). -->
<!-- adapted: the validator and compile commands use the skills/ script paths and the vocabulary paths; the claims validator is the one in skills/write-paper-standalone/scripts; removed the validate_figures.py command because the script needs the original repository (the figure manifest is checked against the schema by hand), see NOTICE -->
---
name: validate-paper
description: Validate grounded paper artifacts and compile LaTeX PDF with strict fail-fast checks.
---

# What the skill is about
Run final quality gate for the generated paper.

# When to use the skill
- After `paper.tex`, `references.bib`, and `claims_grounding.json` are written.
- Before declaring final paper success.

# What to do
1. Validate sections and citation keys:
```bash
python skills/validate-paper/scripts/validate_tex_refs.py \
  --tex {{PAPER_TEX_PATH}} \
  --bib {{PAPER_BIB_PATH}}
```
Require a substantive embedded Supplementary Information section after the bibliography. It must
use `\\appendix` and `\\section{Supplementary Information}`, contain grounded visible content plus
at least one subsection, table, or figure, and contain no template placeholder text.
2. Validate claim mappings, bibliography/provenance consistency, and every `@evidence[[...]]` link in the `scientific-evidence/` bundle:
```bash
python skills/write-paper-standalone/scripts/validate_claims.py \
  --paper-data {{PAPER_DATA_PATH}} \
  --references {{PAPER_REFERENCES_PROVENANCE_PATH}} \
  --bib {{PAPER_BIB_PATH}} \
  --claims {{PAPER_CLAIMS_PATH}} \
  --paper-dir {{PAPER_DIR}}
```
This delegates to the `scientific-evidence` skill verifier, which classifies every `@evidence[[...]]` link as `resolved`, `anchored`, or `dangling`. Any `dangling` link (or an `@evidence[[...]]` token with no `scientific-evidence/` bundle present) is a hard error and must be sent back to the paper-writing agent for repair.
3. Validate figure manifest and figure readability budget when `figure_manifest.json` is present or `paper_strategy.json` requires figures. The canonical schema is `skills/validate-paper/templates/figure_manifest.schema.json`.
4. Compile:
```bash
bash skills/validate-paper/scripts/compile_latex.sh {{PAPER_TEX_PATH}}
```
The compile script also validates that Supplementary Information is visibly rendered on a page
after References with substantive content. Source-only structure checks are not sufficient for
conditional or literal LaTeX constructs.
It records and validates every LaTeX input, restricts BibTeX to the canonical `references.bib`,
and removes inherited Kpathsea path overrides before compiling.
5. Require `paper.pdf` to exist.
6. When `paper_data.json.paper_profile == "standalone"`, also fail on visible paper language that leaks pipeline terms such as `eligible`, `correct and eligible`, `correctness rate`, `worker_01`, `worker_02`, `combined_score`, or similar internal wording.
7. When the selected format or venue guidance requires real author metadata, also fail on anonymous or generic placeholder author blocks and emit soft warnings when title/abstract/introduction/conclusion lack venue-relevant framing or when the discussion drifts into unrelated application narratives.
8. Fail hard on any missing grounding link, missing section, missing/empty/misordered Supplementary Information, unresolved citation, bibliography/provenance mismatch, figure-manifest mismatch, density-budget violation, page-limit violation for formats with enforced budgets, standalone-language violation, author-block violation required by format/venue guidance, dangling `@evidence[[...]]` link, or compile error.
9. Treat the parent orchestrator's validation report as authoritative during repair rounds. Last-body-page fill is rendered geometric vertical occupancy computed from text and graphics bounds, not character count or word density. Do not claim that page validation passed using a character-count proxy. If the canonical validator is unavailable in the agent workspace, make the targeted repair and return control so the parent can compile and revalidate it.
