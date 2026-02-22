Feature: Queue list display width

  When `mdma queue list` renders in TTY mode, every output line must fit
  within the terminal width. Position numbers (e.g. "1.", "8.") are part of
  the display and must be counted inside the column budget — not appended
  outside it. These scenarios reproduce the bug where position numbers are
  prepended after column layout is calculated, causing lines to exceed the
  declared terminal width.

  Background:
    Given the library contains:
      | hash     | artist                 | title                         | bpm | genre     | duration |
      | aa000001 | Sunju Hargun           | Silverhaze (DJ MARIA. Remix)  | 124 | Downtempo | 421      |
      | aa000002 | Carbon Based Lifeforms | Init                          | 118 | Ambient   | 508      |
      | aa000003 | Sunju Hargun           | Right Where It Ends           | 127 | Downtempo | 376      |
      | aa000004 | Carbon Based Lifeforms | Marsa (2026 Remaster)         | 116 | Ambient   | 612      |
      | aa000005 | Sunju Hargun           | Interloper                    | 131 | Downtempo | 293      |
      | aa000006 | Carbon Based Lifeforms | Midnight Traffic Remix        | 122 | Ambient   | 444      |
      | aa000007 | Sunju Hargun           | Polyrytmi                     | 135 | Downtempo | 367      |
      | aa000008 | Carbon Based Lifeforms | 20 Minutes                    | 119 | Ambient   | 1200     |

  # With 8 tracks in the queue the position prefix is "8.  " (4 chars).
  # The natural untruncated table width for this data is:
  #   hash(8) + gap(2) + artist(22) + gap(2) + title(28) + gap(2) + duration(5) = 69 chars
  # At COLUMNS=65, corsett fits the content at 65 chars.
  # Prepending "8.  " makes lines 69 chars, exceeding the 65-column terminal.

  Scenario: Queue list at 65 columns fits within terminal width
    When I run mdma search --artist "Sunju Hargun"
    And I pipe that through mdma queue append
    When I run mdma search --artist "Carbon Based"
    And I pipe that through mdma queue append
    When in tty mode at 65 columns I run mdma queue list
    Then the exit code should be 0
    And the stripped output should contain "1."
    And the stripped output should contain "8."
    And every stripped output line should fit in 65 columns

  Scenario: Queue list at 55 columns fits within terminal width
    When I run mdma search --artist "Sunju Hargun"
    And I pipe that through mdma queue append
    When I run mdma search --artist "Carbon Based"
    And I pipe that through mdma queue append
    When in tty mode at 55 columns I run mdma queue list
    Then the exit code should be 0
    And the stripped output should contain "1."
    And every stripped output line should fit in 55 columns
