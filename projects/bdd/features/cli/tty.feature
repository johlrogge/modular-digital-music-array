Feature: TTY table rendering

  The mdma CLI renders a colored, aligned table when stdout is a terminal.
  Column widths adapt to the terminal width via corsett's resize algorithm.
  These tests verify rendering at various widths.

  Background:
    Given the library contains:
      | hash     | artist                 | title          | bpm | genre     | duration |
      | a1b2c3d4 | Carbon Based Lifeforms | Interloper     | 120 | Ambient   | 432      |
      | e5f6a7b8 | Carbon Based Lifeforms | MOS 6581       | 130 | Downtempo | 389      |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 126 | Ambient   | 502      |
      | b3c4d5e6 | Extrawelt              | Soopertrack    | 132 | Techno    | 445      |
      | f7a8b9c0 | Extrawelt              | Dark Side      | 134 | Techno    | 398      |
      | d1e2f3a4 | Solar Fields           | Sol            | 128 | Ambient   | 612      |

  Scenario: TTY output contains ANSI color codes
    When in tty mode I run mdma search --artist "Solar Fields"
    Then the exit code should be 0
    And the output should have ANSI color codes

  Scenario: TTY search output at default width
    When in tty mode I run mdma search --artist "Solar Fields"
    Then the exit code should be 0
    And the stripped output rows should be:
      | hash     | artist       | title | duration |
      | d1e2f3a4 | Solar Fields | Sol   | 10:12    |

  Scenario: TTY sort preserves order in tty mode
    When I run mdma search --artist "Carbon Based"
    And in tty mode I pipe that through mdma sort title -a
    Then the exit code should be 0
    And the stripped output rows should be:
      | hash     | artist                 | title          | duration |
      | a1b2c3d4 | Carbon Based Lifeforms | Interloper     | 7:12     |
      | e5f6a7b8 | Carbon Based Lifeforms | MOS 6581       | 6:29     |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 8:22     |

  Scenario: Narrow terminal drops duration column
    When in tty mode at 20 columns I run mdma search --artist "Extrawelt"
    Then the exit code should be 0
    And the stripped output should not contain ":"

  Scenario: Wide terminal shows all columns
    When in tty mode at 120 columns I run mdma search --artist "Solar Fields"
    Then the exit code should be 0
    And the stripped output rows should be:
      | hash     | artist       | title | duration |
      | d1e2f3a4 | Solar Fields | Sol   | 10:12    |
