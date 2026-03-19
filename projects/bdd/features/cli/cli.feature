Feature: CLI pipe composition

  The mdma CLI uses pipe-mode output (no colors, no table) when stdout is
  captured. Each line has the format: {8-char-hash}  {artist} - {title}  [{duration}]
  These tests exercise search, sort, queue, and pipe chaining at the binary level.

  Background:
    Given the library contains:
      | hash     | artist                 | title          | bpm | genre     | duration |
      | a1b2c3d4 | Carbon Based Lifeforms | Interloper     | 120 | Ambient   | 432      |
      | e5f6a7b8 | Carbon Based Lifeforms | MOS 6581       | 130 | Downtempo | 389      |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 126 | Ambient   | 502      |
      | b3c4d5e6 | Extrawelt              | Soopertrack    | 132 | Techno    | 445      |
      | f7a8b9c0 | Extrawelt              | Dark Side      | 134 | Techno    | 398      |
      | d1e2f3a4 | Solar Fields           | Sol            | 128 | Ambient   | 612      |

  # --- Search ---

  Scenario: Search by artist returns matching tracks
    When I run mdma search --artist "Carbon Based"
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist                 | title          | duration |
      | a1b2c3d4 | Carbon Based Lifeforms | Interloper     | 7:12     |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 8:22     |
      | e5f6a7b8 | Carbon Based Lifeforms | MOS 6581       | 6:29     |

  Scenario: Search by BPM with tolerance
    When I run mdma search --bpm "130+-4"
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist                 | title          | duration |
      | b3c4d5e6 | Extrawelt              | Soopertrack    | 7:25     |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 8:22     |
      | d1e2f3a4 | Solar Fields           | Sol            | 10:12    |
      | e5f6a7b8 | Carbon Based Lifeforms | MOS 6581       | 6:29     |
      | f7a8b9c0 | Extrawelt              | Dark Side      | 6:38     |

  Scenario: Combined search narrows results
    When I run mdma search --artist "Carbon Based" --bpm "130"
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist                 | title    | duration |
      | e5f6a7b8 | Carbon Based Lifeforms | MOS 6581 | 6:29     |

  Scenario: Pipe output contains track metadata
    When I run mdma search --artist "Solar Fields"
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist       | title | duration |
      | d1e2f3a4 | Solar Fields | Sol   | 10:12    |

  # --- Sort ---

  Scenario: Sort by title ascending
    When I run mdma search --artist "Carbon Based"
    And I pipe that through mdma sort title -a
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist                 | title          | duration |
      | a1b2c3d4 | Carbon Based Lifeforms | Interloper     | 7:12     |
      | e5f6a7b8 | Carbon Based Lifeforms | MOS 6581       | 6:29     |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 8:22     |

  Scenario: Sort by bpm descending
    When I run mdma search --artist "Extrawelt"
    And I pipe that through mdma sort bpm -d
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist    | title       | duration |
      | f7a8b9c0 | Extrawelt | Dark Side   | 6:38     |
      | b3c4d5e6 | Extrawelt | Soopertrack | 7:25     |

  Scenario: Chained stable sort (artist asc, then title asc)
    When I run mdma search --genre "Ambient"
    And I pipe that through mdma sort title -a
    And I pipe that through mdma sort artist -a
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist                 | title          | duration |
      | a1b2c3d4 | Carbon Based Lifeforms | Interloper     | 7:12     |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 8:22     |
      | d1e2f3a4 | Solar Fields           | Sol            | 10:12    |

  # --- Queue operations via CLI ---

  Scenario: Search, sort, append to queue, then list queue
    When I run mdma search --artist "Extrawelt"
    And I pipe that through mdma sort bpm -a
    And I pipe that through mdma queue append
    Then the exit code should be 0
    When I run mdma queue list
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist    | title       | duration |
      | b3c4d5e6 | Extrawelt | Soopertrack | 7:25     |
      | f7a8b9c0 | Extrawelt | Dark Side   | 6:38     |

  Scenario: Queue clear via CLI
    When I run mdma search --artist "Extrawelt"
    And I pipe that through mdma queue append
    When I run mdma queue clear
    Then the exit code should be 0
    When I run mdma queue list
    Then the output should contain 0 lines

  Scenario: Queue list, filter, remove
    When I run mdma search --genre "Techno"
    And I pipe that through mdma queue append
    When I run mdma queue list
    Then the output should contain 2 lines
    When I run mdma queue list
    And I pipe that through mdma search --bpm "132"
    And I pipe that through mdma queue remove
    When I run mdma queue list
    Then the output rows should be:
      | hash     | artist    | title     | duration |
      | f7a8b9c0 | Extrawelt | Dark Side | 6:38     |

  # --- Stdin hash filtering ---

  Scenario: Stdin hash filtering intersects with search
    When I pipe the following through mdma search --artist "Carbon Based":
      """
      a1b2c3d4  dummy line
      c9d0e1f2  another dummy
      d1e2f3a4  not carbon based
      """
    Then the exit code should be 0
    And the output rows should be:
      | hash     | artist                 | title          | duration |
      | a1b2c3d4 | Carbon Based Lifeforms | Interloper     | 7:12     |
      | c9d0e1f2 | Carbon Based Lifeforms | Photosynthesis | 8:22     |
