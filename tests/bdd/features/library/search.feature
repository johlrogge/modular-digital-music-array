Feature: Library search

  Background:
    Given the library contains:
      | artist                 | title         | bpm | genre     |
      | Carbon Based Lifeforms | Interloper    | 120 | Ambient   |
      | Carbon Based Lifeforms | MOS 6581      | 130 | Downtempo |
      | Extrawelt              | Soopertrack   | 132 | Techno    |

  Scenario: Search by artist
    When I search with artist "Carbon Based"
    Then I should find 2 tracks

  Scenario: Search by BPM tolerance
    When I search with bpm 130 tolerance 2
    Then I should find 2 tracks

  Scenario: Combined search
    When I search with artist "Carbon Based" and bpm 130
    Then I should find 1 track

  Scenario: Search by genre
    When I search with genre "Techno"
    Then I should find 1 track

  Scenario: List all tracks
    When I list all tracks
    Then I should find 3 tracks
